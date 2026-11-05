use std::io;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use super::Message;

/// Longest line accepted from the server: 8191 bytes of tags plus a 512-byte
/// body, per the IRCv3 message-tags spec.
const MAX_LINE: usize = 8191 + 512;

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
    Disconnected {
        error: Option<String>,
    },
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

/// Starts a connection on the current tokio runtime. The task ends after
/// sending `Event::Disconnected`, or when every `Handle` sender is dropped.
pub fn spawn(config: Config) -> Handle {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let error = run(config, command_rx, &event_tx).await.err();
        let _ = event_tx.send(Event::Disconnected {
            error: error.map(|e| e.to_string()),
        });
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
) -> io::Result<()> {
    let stream = TcpStream::connect((config.host.as_str(), config.port)).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let _ = events.send(Event::Connected);

    let mut state = Registration::Pending {
        attempted_nick: config.nick.clone(),
    };
    send(&mut writer, &Message::new("NICK", &[&config.nick])).await?;
    send(
        &mut writer,
        &Message::new("USER", &[&config.username, "0", "*", &config.realname]),
    )
    .await?;

    let mut buf = Vec::with_capacity(1024);
    loop {
        tokio::select! {
            read = read_line(&mut reader, &mut buf) => {
                if !read? {
                    return Ok(());
                }
                let line = String::from_utf8_lossy(&buf);
                let Ok(message) = Message::parse(&line) else { continue };
                for reply in respond(&mut state, &config, &message, events) {
                    send(&mut writer, &reply).await?;
                }
                let _ = events.send(Event::Message(message));
            }
            command = commands.recv() => match command {
                Some(Command::Send(message)) => match send(&mut writer, &message).await {
                    Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
                        eprintln!("textual-gtk: {e}");
                    }
                    result => result?,
                },
                Some(Command::Quit(reason)) => {
                    let quit = match reason {
                        Some(reason) => Message::new("QUIT", &[&reason]),
                        None => Message::new("QUIT", &[]),
                    };
                    send(&mut writer, &quit).await?;
                    writer.shutdown().await?;
                    commands.close();
                }
                None => return Ok(()),
            },
        }
    }
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

/// Reads one line into `buf` without its terminator. Returns false at EOF.
/// Bytes past `MAX_LINE` are dropped rather than buffered.
async fn read_line<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> io::Result<bool> {
    buf.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(!buf.is_empty());
        }
        let (chunk, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (&available[..=i], true),
            None => (available, false),
        };
        let room = MAX_LINE.saturating_sub(buf.len());
        buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
        let consumed = chunk.len();
        reader.consume(consumed);
        if done {
            while matches!(buf.last(), Some(b'\n' | b'\r')) {
                buf.pop();
            }
            return Ok(true);
        }
    }
}

async fn send<W: AsyncWriteExt + Unpin>(writer: &mut W, message: &Message) -> io::Result<()> {
    let line = message.to_wire().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to send {}: {e}", message.command),
        )
    })?;
    writer.write_all(line.as_bytes()).await
}
