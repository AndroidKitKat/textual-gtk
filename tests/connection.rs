use std::time::Duration;

use textual_gtk::irc::Message;
use textual_gtk::irc::connection::{self, Command, Config, Disconnect, Event};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::{sleep, timeout};

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

    async fn raw(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    async fn say(&mut self, line: &str) {
        self.raw(format!("{line}\r\n").as_bytes()).await;
    }

    async fn skip_registration(&mut self) {
        assert!(self.expect().await.starts_with("NICK "));
        assert!(self.expect().await.starts_with("USER "));
    }
}

fn config(port: u16, channels: &[&str]) -> Config {
    Config {
        host: "127.0.0.1".into(),
        port,
        nick: "tester".into(),
        username: "tuser".into(),
        realname: "Test User".into(),
        channels: channels.iter().map(|c| (*c).to_owned()).collect(),
    }
}

async fn start(channels: &[&str]) -> (FakeServer, connection::Handle) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = connection::spawn(&tokio::runtime::Handle::current(), config(port, channels));
    let (socket, _) = listener.accept().await.unwrap();
    let (reader, writer) = socket.into_split();
    let server = FakeServer {
        lines: BufReader::new(reader).lines(),
        writer,
    };
    (server, handle)
}

async fn next_event(handle: &mut connection::Handle) -> Event {
    timeout(Duration::from_secs(10), handle.events.recv())
        .await
        .expect("no event")
        .expect("event channel closed")
}

async fn next_message(handle: &mut connection::Handle) -> Message {
    loop {
        match next_event(handle).await {
            Event::Message(m) => return m,
            Event::Disconnected(reason) => panic!("disconnected: {reason:?}"),
            _ => continue,
        }
    }
}

async fn disconnect_reason(handle: &mut connection::Handle) -> Disconnect {
    loop {
        if let Event::Disconnected(reason) = next_event(handle).await {
            return reason;
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
    loop {
        match next_event(&mut handle).await {
            Event::Registered { nick } => break assert_eq!(nick, "tester_"),
            Event::Message(_) => continue,
            other => panic!("expected Registered, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn answers_ping_and_forwards_every_message() {
    let (mut server, mut handle) = start(&[]).await;
    server.skip_registration().await;

    server.say("PING :abc123").await;
    assert_eq!(server.expect().await, "PONG abc123");

    server.say(":alice!a@host PRIVMSG #textual :hi there").await;
    let mut forwarded = next_message(&mut handle).await;
    while forwarded.command != "PRIVMSG" {
        forwarded = next_message(&mut handle).await;
    }
    assert_eq!(forwarded.source().unwrap().nick, "alice");
    assert_eq!(forwarded.params, ["#textual", "hi there"]);
}

#[tokio::test]
async fn a_command_arriving_mid_line_does_not_corrupt_that_line() {
    let (mut server, mut handle) = start(&[]).await;
    server.skip_registration().await;

    server.raw(b":a!b@c PRIVMSG #x :hel").await;
    sleep(Duration::from_millis(100)).await;
    handle
        .commands
        .send(Command::Send(Message::new("PRIVMSG", &["#x", "typing"])))
        .unwrap();
    assert_eq!(server.expect().await, "PRIVMSG #x typing");
    server.raw(b"lo world\r\n").await;

    let message = next_message(&mut handle).await;
    assert_eq!(message.command, "PRIVMSG");
    assert_eq!(message.params, ["#x", "hello world"]);
}

#[tokio::test]
async fn overlong_and_unterminated_lines_are_dropped_not_truncated() {
    let (mut server, mut handle) = start(&[]).await;
    server.skip_registration().await;

    let long = format!(":a!b@c PRIVMSG #x :{}\r\n", "y".repeat(20_000));
    server.raw(long.as_bytes()).await;
    server.say(":a!b@c PRIVMSG #x :after").await;
    assert_eq!(next_message(&mut handle).await.params, ["#x", "after"]);

    server.raw(b":a!b@c PRIVMSG #x :cut off mid").await;
    drop(server);
    assert_eq!(
        disconnect_reason(&mut handle).await,
        Disconnect::ServerClosed
    );
}

#[tokio::test]
async fn refused_lines_are_skipped_without_disconnecting() {
    let (mut server, mut handle) = start(&[]).await;
    server.skip_registration().await;

    let evil = Message::new("PRIVMSG", &["#textual", "hello\r\nQUIT :pwned"]);
    handle.commands.send(Command::Send(evil)).unwrap();
    server.say("PING :a\rb").await;
    let fine = Message::new("PRIVMSG", &["#textual", "still here"]);
    handle.commands.send(Command::Send(fine)).unwrap();

    assert_eq!(server.expect().await, "PRIVMSG #textual :still here");
    server.say("PING :ok").await;
    assert_eq!(server.expect().await, "PONG ok");
    assert_eq!(next_event(&mut handle).await, Event::Connected);
}

#[tokio::test]
async fn quit_waits_for_the_servers_goodbye() {
    let (mut server, mut handle) = start(&[]).await;
    server.skip_registration().await;

    handle
        .commands
        .send(Command::Quit(Some("bye now".into())))
        .unwrap();
    handle
        .commands
        .send(Command::Send(Message::new("PRIVMSG", &["#x", "late"])))
        .unwrap();
    assert_eq!(server.expect().await, "QUIT :bye now");
    server.say("ERROR :Closing Link").await;
    drop(server);

    let goodbye = next_message(&mut handle).await;
    assert_eq!(goodbye.command, "ERROR");
    assert_eq!(disconnect_reason(&mut handle).await, Disconnect::UserQuit);
}

#[tokio::test]
async fn a_quit_reason_with_a_line_break_still_quits() {
    let (mut server, handle) = start(&[]).await;
    server.skip_registration().await;
    handle
        .commands
        .send(Command::Quit(Some("bye\r\nJOIN #x".into())))
        .unwrap();
    assert_eq!(server.expect().await, "QUIT");
}

#[tokio::test]
async fn server_hangup_reports_server_closed() {
    let (mut server, mut handle) = start(&[]).await;
    server.skip_registration().await;
    drop(server);
    assert_eq!(
        disconnect_reason(&mut handle).await,
        Disconnect::ServerClosed
    );
}

#[tokio::test]
async fn refused_connection_reports_the_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut handle = connection::spawn(&tokio::runtime::Handle::current(), config(port, &[]));
    match disconnect_reason(&mut handle).await {
        Disconnect::Error(e) => assert!(e.contains("refused"), "{e}"),
        other => panic!("expected a refused connection, got {other:?}"),
    }
}

#[tokio::test]
async fn quit_cancels_a_hanging_connect() {
    let mut handle = connection::spawn(
        &tokio::runtime::Handle::current(),
        Config {
            host: "10.255.255.1".into(),
            ..config(6667, &[])
        },
    );
    sleep(Duration::from_millis(200)).await;
    handle.commands.send(Command::Quit(None)).unwrap();
    let reason = timeout(Duration::from_secs(2), disconnect_reason(&mut handle))
        .await
        .expect("connect was not cancelled");
    assert_eq!(reason, Disconnect::UserQuit);
}

/// Needs a real server: `TEXTUAL_TEST_IRC=127.0.0.1:6667 cargo test -- --ignored`.
#[tokio::test]
#[ignore]
async fn two_clients_talk_through_a_real_server() {
    let addr = std::env::var("TEXTUAL_TEST_IRC").unwrap_or("127.0.0.1:6667".into());
    let (host, port) = addr.rsplit_once(':').unwrap();
    let runtime = tokio::runtime::Handle::current();
    let connect = |nick: &str| {
        connection::spawn(
            &runtime,
            Config {
                host: host.into(),
                port: port.parse().unwrap(),
                nick: nick.into(),
                username: nick.into(),
                realname: nick.into(),
                channels: vec!["#textual-test".into()],
            },
        )
    };
    async fn joined(handle: &mut connection::Handle) {
        while next_message(handle).await.command != "366" {}
    }
    let mut listener = connect("listener");
    joined(&mut listener).await;
    let mut talker = connect("talker");
    joined(&mut talker).await;

    talker
        .commands
        .send(Command::Send(Message::new(
            "PRIVMSG",
            &["#textual-test", "hello from talker"],
        )))
        .unwrap();
    let mut message = next_message(&mut listener).await;
    while message.command != "PRIVMSG" {
        message = next_message(&mut listener).await;
    }
    assert!(message.source().unwrap().nick.starts_with("talker"));
    assert_eq!(message.params, ["#textual-test", "hello from talker"]);

    talker.commands.send(Command::Quit(None)).unwrap();
    assert_eq!(disconnect_reason(&mut talker).await, Disconnect::UserQuit);
    listener.commands.send(Command::Quit(None)).unwrap();
}
