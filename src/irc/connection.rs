use std::io;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until, timeout};

use super::{Message, WireError};

/// Longest line accepted from the server: 8191 bytes of tags plus a 512-byte
/// body, per the IRCv3 message-tags spec, plus the CRLF.
const MAX_LINE: usize = 8191 + 512 + 2;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to keep reading after QUIT, so the server's ERROR reply arrives.
const QUIT_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub nick: String,
    pub username: String,
    pub realname: String,
    pub channels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Connected,
    /// The server accepted registration (001) under this nick.
    Registered {
        nick: String,
    },
    Message(Message),
    /// A `Command::Send` that could not go on the wire, and why.
    Refused {
        message: Message,
        reason: WireError,
    },
    /// Always the last event.
    Disconnected(Disconnect),
}

/// Why a connection ended, so the app can tell whether to reconnect.
#[derive(Debug, Clone, PartialEq)]
pub enum Disconnect {
    UserQuit,
    ServerClosed,
    Error(String),
}

#[derive(Debug, Clone)]
pub enum Command {
    Send(Message),
    Quit(Option<String>),
}

pub struct Handle {
    pub commands: mpsc::UnboundedSender<Command>,
    pub events: mpsc::UnboundedReceiver<Event>,
}

/// Starts a connection on `runtime`. The task ends after sending
/// `Event::Disconnected`.
pub fn spawn(runtime: &tokio::runtime::Handle, config: Config) -> Handle {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    runtime.spawn(async move {
        let reason = match run(config, command_rx, &event_tx).await {
            Ok(reason) => reason,
            Err(e) => Disconnect::Error(e.to_string()),
        };
        let _ = event_tx.send(Event::Disconnected(reason));
    });
    Handle {
        commands: command_tx,
        events: event_rx,
    }
}

enum Registration {
    Pending { attempted_nick: String },
    Done,
}

async fn run(
    config: Config,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: &mpsc::UnboundedSender<Event>,
) -> io::Result<Disconnect> {
    if config.nick.is_empty() || config.nick.contains(' ') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bad nick or user name: a nick cannot be empty or contain spaces",
        ));
    }
    let registration = [
        Message::new("NICK", &[&config.nick]),
        Message::new("USER", &[&config.username, "0", "*", &config.realname]),
    ]
    .iter()
    .map(Message::to_wire)
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("bad nick or user name: {e}"),
        )
    })?;

    let connect = timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((config.host.as_str(), config.port)),
    );
    let stream = tokio::select! {
        result = connect => match result {
            Ok(stream) => stream?,
            Err(_) => return Err(io::Error::new(io::ErrorKind::TimedOut, "connection timed out")),
        },
        () = quit_requested(&mut commands) => return Ok(Disconnect::UserQuit),
    };
    let (reader, mut writer) = stream.into_split();
    let mut reader = LineReader::new(reader);
    if events.send(Event::Connected).is_err() {
        return Ok(Disconnect::UserQuit);
    }

    let mut state = Registration::Pending {
        attempted_nick: config.nick.clone(),
    };
    for line in &registration {
        writer.write_all(line.as_bytes()).await?;
    }

    let mut quit_deadline: Option<Instant> = None;
    loop {
        let step: io::Result<Option<Disconnect>> = tokio::select! {
            line = reader.next_line() => match line {
                Ok(Some(line)) => on_line(&line, &mut state, &config, &mut writer, events).await,
                Ok(None) if quit_deadline.is_some() => Ok(Some(Disconnect::UserQuit)),
                Ok(None) => Ok(Some(Disconnect::ServerClosed)),
                Err(e) => Err(e),
            },
            command = commands.recv(), if quit_deadline.is_none() => {
                let (quit, result) = on_command(command, &mut writer, events).await;
                if quit {
                    quit_deadline = Some(Instant::now() + QUIT_GRACE);
                }
                result.map(|()| None)
            }
            () = sleep_until(quit_deadline.unwrap_or_else(Instant::now)), if quit_deadline.is_some() => {
                Ok(Some(Disconnect::UserQuit))
            }
        };
        match step {
            Ok(None) => {}
            Ok(Some(reason)) => return Ok(reason),
            Err(_) if quit_deadline.is_some() => return Ok(Disconnect::UserQuit),
            Err(e) => return Err(e),
        }
    }
}

async fn on_line<W: AsyncWriteExt + Unpin>(
    line: &str,
    state: &mut Registration,
    config: &Config,
    writer: &mut W,
    events: &mpsc::UnboundedSender<Event>,
) -> io::Result<Option<Disconnect>> {
    let Ok(message) = Message::parse(line) else {
        return Ok(None);
    };
    for reply in respond(state, config, &message, events) {
        match reply.to_wire() {
            Ok(wire) => writer.write_all(wire.as_bytes()).await?,
            Err(e) => eprintln!("textual-gtk: not sending {}: {e}", reply.command),
        }
    }
    if events.send(Event::Message(message)).is_err() {
        return Ok(Some(Disconnect::UserQuit));
    }
    Ok(None)
}

/// Returns whether the user asked to quit (or dropped every sender, which
/// means the same), and the result of writing.
async fn on_command<W: AsyncWriteExt + Unpin>(
    command: Option<Command>,
    writer: &mut W,
    events: &mpsc::UnboundedSender<Event>,
) -> (bool, io::Result<()>) {
    let (quit, message) = match command {
        Some(Command::Send(message)) => (false, message),
        Some(Command::Quit(reason)) => (true, quit_message(reason)),
        None => (true, quit_message(None)),
    };
    let result = match message.to_wire() {
        Ok(wire) => writer.write_all(wire.as_bytes()).await,
        Err(reason) => {
            let _ = events.send(Event::Refused { message, reason });
            Ok(())
        }
    };
    (quit, result)
}

/// Resolves once the user asks to quit or drops the handle. Sends queued
/// before the socket exists are discarded.
async fn quit_requested(commands: &mut mpsc::UnboundedReceiver<Command>) {
    while let Some(command) = commands.recv().await {
        if let Command::Quit(_) = command {
            return;
        }
    }
}

/// A reason that cannot go on the wire falls back to a bare QUIT, so the
/// server still sees a clean quit.
fn quit_message(reason: Option<String>) -> Message {
    let with_reason = reason.map(|r| Message::new("QUIT", &[&r]));
    with_reason
        .filter(|m| m.to_wire().is_ok())
        .unwrap_or_else(|| Message::new("QUIT", &[]))
}

/// Protocol replies the connection owns itself: keepalive, nick collisions
/// during registration, and autojoin once registered.
fn respond(
    state: &mut Registration,
    config: &Config,
    message: &Message,
    events: &mpsc::UnboundedSender<Event>,
) -> Vec<Message> {
    match (message.command.as_str(), &mut *state) {
        ("PING", _) => vec![Message {
            command: "PONG".into(),
            params: message.params.clone(),
            ..Default::default()
        }],
        ("433", Registration::Pending { attempted_nick }) => {
            attempted_nick.push('_');
            vec![Message::new("NICK", &[attempted_nick.as_str()])]
        }
        ("001", Registration::Pending { attempted_nick }) => {
            let nick = message
                .param(0)
                .map_or_else(|| attempted_nick.clone(), str::to_owned);
            *state = Registration::Done;
            let _ = events.send(Event::Registered { nick });
            if config.channels.is_empty() {
                Vec::new()
            } else {
                vec![Message::new("JOIN", &[&config.channels.join(",")])]
            }
        }
        _ => Vec::new(),
    }
}

/// Splits a byte stream into lines. Safe to cancel between calls (as
/// `select!` does): a half-read line stays in `buf` for the next call.
struct LineReader<R> {
    inner: BufReader<R>,
    buf: Vec<u8>,
    overflowed: bool,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    fn new(inner: R) -> Self {
        LineReader {
            inner: BufReader::new(inner),
            buf: Vec::with_capacity(1024),
            overflowed: false,
        }
    }

    /// The next complete line without its terminator, or `None` at EOF.
    /// Lines longer than `MAX_LINE`, and a final line with no terminator,
    /// are dropped rather than passed on truncated.
    async fn next_line(&mut self) -> io::Result<Option<String>> {
        loop {
            let available = self.inner.fill_buf().await?;
            if available.is_empty() {
                return Ok(None);
            }
            let (chunk, done) = match available.iter().position(|&b| b == b'\n') {
                Some(i) => (&available[..=i], true),
                None => (available, false),
            };
            if self.buf.len() + chunk.len() > MAX_LINE {
                self.overflowed = true;
                self.buf.clear();
            } else if !self.overflowed {
                self.buf.extend_from_slice(chunk);
            }
            let consumed = chunk.len();
            self.inner.consume(consumed);
            if !done {
                continue;
            }
            let overflowed = std::mem::take(&mut self.overflowed);
            let mut line = std::mem::take(&mut self.buf);
            if overflowed {
                continue;
            }
            while matches!(line.last(), Some(b'\n' | b'\r')) {
                line.pop();
            }
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
    }
}
