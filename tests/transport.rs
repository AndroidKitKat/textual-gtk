use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use textual_core::irc::{Message, WireLine};
use textual_core::model::network::{Address, CloseReason, ConnId, Tls, TransportEvent};
use textual_gtk::io::transport::{self, Connection, Pacing, Settings};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep, timeout};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::{self, ServerConfig};

const CONN: ConnId = ConnId::new(7);

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<S: AsyncRead + AsyncWrite + Unpin + Send> Stream for S {}

struct FakeServer {
    lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<Box<dyn Stream>>>>,
    writer: tokio::io::WriteHalf<Box<dyn Stream>>,
}

impl FakeServer {
    fn new(stream: Box<dyn Stream>) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        FakeServer {
            lines: BufReader::new(reader).lines(),
            writer,
        }
    }

    async fn expect(&mut self) -> String {
        timeout(Duration::from_secs(5), self.lines.next_line())
            .await
            .expect("client went quiet")
            .unwrap()
            .expect("client closed")
    }

    async fn raw(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    async fn say(&mut self, line: &str) {
        self.raw(format!("{line}\r\n").as_bytes()).await;
    }
}

type Events = mpsc::UnboundedReceiver<(ConnId, TransportEvent)>;

async fn listen() -> (TcpListener, Address) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let address = Address {
        host: "localhost".into(),
        port,
    };
    (listener, address)
}

fn dial(address: Address, tls: Tls, settings: Settings) -> (Connection, Events) {
    let (tx, rx) = mpsc::unbounded_channel();
    let runtime = tokio::runtime::Handle::current();
    (
        transport::spawn(&runtime, CONN, address, tls, settings, tx),
        rx,
    )
}

/// A plain connection, accepted and opened.
async fn start(settings: Settings) -> (FakeServer, Connection, Events) {
    let (listener, address) = listen().await;
    let (conn, mut events) = dial(address, Tls::Off, settings);
    let (socket, _) = listener.accept().await.unwrap();
    assert_eq!(next(&mut events).await, TransportEvent::Opened);
    (FakeServer::new(Box::new(socket)), conn, events)
}

async fn next(events: &mut Events) -> TransportEvent {
    let (conn, event) = timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("no event")
        .expect("event channel closed");
    assert_eq!(conn, CONN, "every event carries its ConnId");
    event
}

async fn next_line(events: &mut Events) -> Message {
    match next(events).await {
        TransportEvent::Line(m) => m,
        other => panic!("expected a line, got {other:?}"),
    }
}

fn wire(text: &str) -> WireLine {
    let msg = Message::parse(text).unwrap();
    msg.to_wire().unwrap()
}

fn self_signed() -> (CertificateDer<'static>, TlsAcceptor) {
    let key = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let cert = key.cert.der().clone();
    let private = PrivatePkcs8KeyDer::from(key.signing_key.serialize_der());
    let config = ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.clone()], private.into())
    .unwrap();
    (cert, TlsAcceptor::from(Arc::new(config)))
}

#[tokio::test]
async fn lines_are_framed_across_reads_and_parsed() {
    let (mut server, _conn, mut events) = start(Settings::default()).await;
    server.raw(b"PRIVMSG a :one\r\nPRIV").await;
    sleep(Duration::from_millis(50)).await;
    server.raw(b"MSG b :two\n\r\n:x NOTICE c :three\r\n").await;
    assert_eq!(next_line(&mut events).await.params, ["a", "one"]);
    assert_eq!(next_line(&mut events).await.params, ["b", "two"]);
    let third = next_line(&mut events).await;
    assert_eq!(
        (third.command.as_str(), third.params[1].as_str()),
        ("NOTICE", "three")
    );
}

#[tokio::test]
async fn overlong_and_unterminated_lines_are_dropped_not_truncated() {
    let (mut server, _conn, mut events) = start(Settings::default()).await;
    let mut long = vec![b'a'; 9000];
    long.extend_from_slice(b"\r\n");
    server.raw(&long).await;
    server.say("PRIVMSG a :after").await;
    server.raw(b"PRIVMSG a :unterminated").await;
    drop(server);
    assert_eq!(next_line(&mut events).await.params, ["a", "after"]);
    assert_eq!(
        next(&mut events).await,
        TransportEvent::Closed(CloseReason::Eof)
    );
}

#[tokio::test]
async fn ping_is_answered_at_once_and_still_reported() {
    let pacing = Pacing {
        burst: 2,
        interval: Duration::from_secs(60),
    };
    let (mut server, conn, mut events) = start(Settings {
        pacing,
        ..Settings::default()
    })
    .await;
    for n in 1..=4 {
        conn.send(wire(&format!("PRIVMSG #c :{n}")));
    }
    assert_eq!(server.expect().await, "PRIVMSG #c 1");
    assert_eq!(server.expect().await, "PRIVMSG #c 2");
    server.say("PING :tok en").await;
    assert_eq!(
        server.expect().await,
        "PONG :tok en",
        "PONG skips the queue"
    );
    let ping = next_line(&mut events).await;
    assert_eq!(
        (ping.command.as_str(), ping.params[0].as_str()),
        ("PING", "tok en")
    );
}

#[tokio::test]
async fn queued_lines_go_out_in_order_and_paced() {
    let pacing = Pacing {
        burst: 3,
        interval: Duration::from_millis(400),
    };
    let (mut server, conn, _events) = start(Settings {
        pacing,
        ..Settings::default()
    })
    .await;
    let start = Instant::now();
    for n in 1..=6 {
        conn.send(wire(&format!("PRIVMSG #c :{n}")));
    }
    for n in 1..=3 {
        assert_eq!(server.expect().await, format!("PRIVMSG #c {n}"));
    }
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "the burst waits"
    );
    for n in 4..=6 {
        assert_eq!(server.expect().await, format!("PRIVMSG #c {n}"));
    }
    let took = start.elapsed();
    assert!(took >= Duration::from_millis(400), "not paced: {took:?}");
}

#[tokio::test]
async fn quit_skips_the_paced_queue() {
    let pacing = Pacing {
        burst: 1,
        interval: Duration::from_secs(60),
    };
    let (mut server, conn, _events) = start(Settings {
        pacing,
        ..Settings::default()
    })
    .await;
    for n in 1..=3 {
        conn.send(wire(&format!("PRIVMSG #c :{n}")));
    }
    assert_eq!(server.expect().await, "PRIVMSG #c 1");
    conn.send(wire("QUIT :bye"));
    assert_eq!(
        server.expect().await,
        "QUIT bye",
        "QUIT waited behind the queue"
    );
}

#[tokio::test]
async fn a_server_hangup_is_eof() {
    let (server, _conn, mut events) = start(Settings::default()).await;
    drop(server);
    assert_eq!(
        next(&mut events).await,
        TransportEvent::Closed(CloseReason::Eof)
    );
}

#[tokio::test]
async fn a_refused_connection_reports_the_error() {
    let (listener, mut address) = listen().await;
    drop(listener);
    address.host = "127.0.0.1".into();
    let (_conn, mut events) = dial(address, Tls::Off, Settings::default());
    match next(&mut events).await {
        TransportEvent::Closed(CloseReason::Error(e)) => assert!(e.contains("refused"), "{e}"),
        other => panic!("expected a refused connection, got {other:?}"),
    }
}

#[tokio::test]
async fn the_connect_timeout_covers_the_tls_handshake() {
    let (listener, address) = listen().await;
    let settings = Settings {
        connect_timeout: Duration::from_millis(300),
        ..Settings::default()
    };
    let (_conn, mut events) = dial(address, Tls::On { verify: true }, settings);
    let _silent = listener.accept().await.unwrap();
    assert_eq!(
        next(&mut events).await,
        TransportEvent::Closed(CloseReason::ConnectTimeout)
    );
}

#[tokio::test]
async fn hangup_cancels_a_dial_and_reports_nothing() {
    let (listener, address) = listen().await;
    let (conn, mut events) = dial(address, Tls::On { verify: true }, Settings::default());
    let _silent = listener.accept().await.unwrap();
    conn.hangup();
    let after = timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("the task did not end");
    assert_eq!(after, None, "no event after a hangup");
}

#[tokio::test]
async fn hangup_closes_an_open_connection() {
    let (mut server, conn, mut events) = start(Settings::default()).await;
    conn.hangup();
    let read = timeout(Duration::from_secs(2), server.lines.next_line())
        .await
        .expect("the socket stayed open");
    assert!(matches!(read, Ok(None) | Err(_)), "{read:?}");
    assert_eq!(events.recv().await, None);
}

#[tokio::test]
async fn an_untrusted_certificate_is_rejected_with_its_fingerprint() {
    let (listener, address) = listen().await;
    let (cert, acceptor) = self_signed();
    let (_conn, mut events) = dial(address, Tls::On { verify: true }, Settings::default());
    let (socket, _) = listener.accept().await.unwrap();
    let _ = acceptor.accept(socket).await;
    let sha256: [u8; 32] = Sha256::digest(cert.as_ref()).into();
    assert_eq!(
        next(&mut events).await,
        TransportEvent::Closed(CloseReason::BadCertificate { sha256 })
    );
}

#[tokio::test]
async fn verify_off_accepts_a_self_signed_certificate() {
    let (listener, address) = listen().await;
    let (_cert, acceptor) = self_signed();
    let (conn, mut events) = dial(address, Tls::On { verify: false }, Settings::default());
    let (socket, _) = listener.accept().await.unwrap();
    let tls = acceptor.accept(socket).await.unwrap();
    let mut server = FakeServer::new(Box::new(tls));
    assert_eq!(next(&mut events).await, TransportEvent::Opened);
    conn.send(wire("CAP LS 302"));
    assert_eq!(server.expect().await, "CAP LS 302");
    server.say("PING :over-tls").await;
    assert_eq!(server.expect().await, "PONG over-tls");
    assert_eq!(next_line(&mut events).await.command, "PING");
}

/// Needs Ergo's TLS port (self-signed): `scripts/test-server.sh`, then
/// `cargo test --test transport -- --ignored`.
#[tokio::test]
#[ignore]
async fn ergo_tls_port() {
    let address = Address {
        host: "127.0.0.1".into(),
        port: 6697,
    };
    let (_conn, mut events) = dial(
        address.clone(),
        Tls::On { verify: true },
        Settings::default(),
    );
    assert!(
        matches!(
            next(&mut events).await,
            TransportEvent::Closed(CloseReason::BadCertificate { .. })
        ),
        "a self-signed certificate must not verify"
    );

    let (conn, mut events) = dial(address, Tls::On { verify: false }, Settings::default());
    assert_eq!(next(&mut events).await, TransportEvent::Opened);
    conn.send(wire("CAP LS 302"));
    loop {
        if next_line(&mut events).await.command == "CAP" {
            break;
        }
    }
}

/// The model and the transport together, against Ergo's TLS port: register,
/// go online, quit. Run as `ergo_tls_port` is.
#[tokio::test]
#[ignore]
async fn a_network_registers_on_ergo_over_tls_and_quits() {
    use std::collections::HashMap;
    use textual_core::irc::caps::WANTED;
    use textual_core::model::network::{Effect, Input, Link, Network, ServerConfig, ServerId};
    use textual_core::model::registration::{Auth, Setup};

    let mut net = Network::new(ServerConfig {
        id: ServerId(1),
        addresses: vec![Address {
            host: "127.0.0.1".into(),
            port: 6697,
        }],
        tls: Tls::On { verify: false },
        auto_reconnect: true,
        setup: Setup {
            nick: "lifecycle".into(),
            alternates: Vec::new(),
            username: "lifecycle".into(),
            realname: "Textual".into(),
            auth: Auth::None,
            server_password: None,
            account_password: None,
        },
        wanted_caps: WANTED.iter().map(|c| (*c).to_owned()).collect(),
    });
    let (tx, mut rx) = mpsc::unbounded_channel();
    let runtime = tokio::runtime::Handle::current();
    let mut conns: HashMap<ConnId, Connection> = HashMap::new();
    let mut input = Input::Connect;
    let mut quit_sent = false;
    loop {
        let mut out = Vec::new();
        net.update(input, std::time::Instant::now(), &mut out);
        for effect in out {
            match effect {
                Effect::Dial { conn, address, tls } => {
                    let c = transport::spawn(
                        &runtime,
                        conn,
                        address,
                        tls,
                        Settings::default(),
                        tx.clone(),
                    );
                    conns.insert(conn, c);
                }
                Effect::Send { conn, line } => conns[&conn].send(line),
                Effect::Hangup { conn } => drop(conns.remove(&conn)),
                Effect::Notice(_) => {}
            }
        }
        match net.link() {
            Link::Online { session, .. } if !quit_sent => {
                assert_eq!(session.nick(), "lifecycle");
                assert!(session.caps().is_enabled("message-tags"));
                quit_sent = true;
                input = Input::Disconnect {
                    reason: Some("done".into()),
                };
                continue;
            }
            Link::Offline { last } if quit_sent => {
                assert_eq!(*last, Some(textual_core::model::network::Ended::UserQuit));
                return;
            }
            Link::Offline { .. } | Link::Waiting { .. } if !quit_sent => {
                panic!("the link ended: {:?}", net.link());
            }
            _ => {}
        }
        let (conn, event) = timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("Ergo went quiet")
            .expect("events");
        input = Input::Transport { conn, event };
    }
}
