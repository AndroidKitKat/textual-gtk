use std::time::Duration;

use textual_gtk::irc::Message;
use textual_gtk::irc::connection::{self, Command, Config, Event};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::timeout;

struct FakeServer {
    lines: tokio::io::Lines<BufReader<OwnedReadHalf>>,
    writer: OwnedWriteHalf,
}

impl FakeServer {
    async fn expect(&mut self) -> String {
        timeout(Duration::from_secs(5), self.lines.next_line())
            .await
            .expect("client went quiet")
            .unwrap()
            .expect("client closed")
    }

    async fn say(&mut self, line: &str) {
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .unwrap();
    }
}

async fn start(channels: &[&str]) -> (FakeServer, connection::Handle) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = connection::spawn(Config {
        host: "127.0.0.1".into(),
        port,
        nick: "tester".into(),
        username: "tuser".into(),
        realname: "Test User".into(),
        channels: channels.iter().map(|c| (*c).to_owned()).collect(),
    });
    let (socket, _) = listener.accept().await.unwrap();
    let (reader, writer) = socket.into_split();
    let server = FakeServer {
        lines: BufReader::new(reader).lines(),
        writer,
    };
    (server, handle)
}

async fn next_event(handle: &mut connection::Handle) -> Event {
    timeout(Duration::from_secs(5), handle.events.recv())
        .await
        .expect("no event")
        .expect("event channel closed")
}

async fn next_non_message(handle: &mut connection::Handle) -> Event {
    loop {
        match next_event(handle).await {
            Event::Message(_) => continue,
            other => return other,
        }
    }
}

#[tokio::test]
async fn registers_retries_taken_nick_and_autojoins() {
    let (mut server, mut handle) = start(&["#textual", "#gtk"]).await;
    assert_eq!(next_event(&mut handle).await, Event::Connected);
    assert_eq!(server.expect().await, "NICK tester");
    assert_eq!(server.expect().await, "USER tuser 0 * :Test User");

    server
        .say(":srv 433 * tester :Nickname is already in use")
        .await;
    assert_eq!(server.expect().await, "NICK tester_");

    server.say(":srv 001 tester_ :Welcome").await;
    assert_eq!(server.expect().await, "JOIN #textual,#gtk");
    assert_eq!(
        next_non_message(&mut handle).await,
        Event::Registered {
            nick: "tester_".into()
        }
    );
}

#[tokio::test]
async fn answers_ping_and_forwards_every_message() {
    let (mut server, mut handle) = start(&[]).await;
    server.expect().await;
    server.expect().await;

    server.say("PING :abc123").await;
    assert_eq!(server.expect().await, "PONG abc123");

    server.say(":alice!a@host PRIVMSG #textual :hi there").await;
    let forwarded = loop {
        if let Event::Message(m) = next_event(&mut handle).await
            && m.command == "PRIVMSG"
        {
            break m;
        }
    };
    assert_eq!(forwarded.source().unwrap().nick, "alice");
    assert_eq!(forwarded.params, ["#textual", "hi there"]);
}

#[tokio::test]
async fn refuses_embedded_line_breaks_without_disconnecting() {
    let (mut server, handle) = start(&[]).await;
    server.expect().await;
    server.expect().await;

    let evil = Message::new("PRIVMSG", &["#textual", "hello\r\nQUIT :pwned"]);
    handle.commands.send(Command::Send(evil)).unwrap();
    let fine = Message::new("PRIVMSG", &["#textual", "still here"]);
    handle.commands.send(Command::Send(fine)).unwrap();

    assert_eq!(server.expect().await, "PRIVMSG #textual :still here");
}

#[tokio::test]
async fn quit_sends_reason_and_reports_clean_disconnect() {
    let (mut server, mut handle) = start(&[]).await;
    assert_eq!(next_event(&mut handle).await, Event::Connected);
    server.expect().await;
    server.expect().await;

    handle
        .commands
        .send(Command::Quit(Some("bye now".into())))
        .unwrap();
    assert_eq!(server.expect().await, "QUIT :bye now");
    assert_eq!(
        next_non_message(&mut handle).await,
        Event::Disconnected { error: None }
    );
}

#[tokio::test]
async fn server_hangup_reports_disconnect() {
    let (mut server, mut handle) = start(&[]).await;
    server.expect().await;
    server.expect().await;
    drop(server);
    assert_eq!(next_event(&mut handle).await, Event::Connected);
    assert_eq!(
        next_non_message(&mut handle).await,
        Event::Disconnected { error: None }
    );
}

#[tokio::test]
async fn refused_connection_reports_the_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut handle = connection::spawn(Config {
        host: "127.0.0.1".into(),
        port,
        nick: "n".into(),
        username: "u".into(),
        realname: "r".into(),
        channels: vec![],
    });
    match next_event(&mut handle).await {
        Event::Disconnected { error: Some(e) } => assert!(e.contains("refused"), "{e}"),
        other => panic!("expected a refused connection, got {other:?}"),
    }
}

/// Needs a real server: `TEXTUAL_TEST_IRC=127.0.0.1:6667 cargo test -- --ignored`.
#[tokio::test]
#[ignore]
async fn two_clients_talk_through_a_real_server() {
    let addr = std::env::var("TEXTUAL_TEST_IRC").unwrap_or("127.0.0.1:6667".into());
    let (host, port) = addr.rsplit_once(':').unwrap();
    let config = |nick: &str| Config {
        host: host.into(),
        port: port.parse().unwrap(),
        nick: nick.into(),
        username: nick.into(),
        realname: nick.into(),
        channels: vec!["#textual-test".into()],
    };
    let joined = |handle: &mut connection::Handle| {
        let mut events =
            std::mem::replace(&mut handle.events, tokio::sync::mpsc::unbounded_channel().1);
        async move {
            loop {
                match timeout(Duration::from_secs(10), events.recv()).await {
                    Ok(Some(Event::Message(m))) if m.command == "366" => return events,
                    Ok(Some(Event::Disconnected { error })) => panic!("disconnected: {error:?}"),
                    Ok(Some(_)) => continue,
                    other => panic!("never joined: {other:?}"),
                }
            }
        }
    };
    let mut listener = connection::spawn(config("listener"));
    listener.events = joined(&mut listener).await;
    let mut talker = connection::spawn(config("talker"));
    talker.events = joined(&mut talker).await;

    talker
        .commands
        .send(Command::Send(Message::new(
            "PRIVMSG",
            &["#textual-test", "hello from talker"],
        )))
        .unwrap();
    loop {
        match timeout(Duration::from_secs(10), listener.events.recv()).await {
            Ok(Some(Event::Message(m))) if m.command == "PRIVMSG" => {
                assert!(m.source().unwrap().nick.starts_with("talker"));
                assert_eq!(m.params, ["#textual-test", "hello from talker"]);
                break;
            }
            Ok(Some(_)) => continue,
            other => panic!("message never arrived: {other:?}"),
        }
    }
    talker.commands.send(Command::Quit(None)).unwrap();
    listener.commands.send(Command::Quit(None)).unwrap();
}
