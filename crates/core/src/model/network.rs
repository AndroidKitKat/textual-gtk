//! One configured server and the lifecycle of its connection: dial,
//! register, stay online, end, and decide whether and when to try again.
//!
//! `Network::update` is the only way in. It takes an `Input` and the current
//! time and pushes `Effect`s for the app to carry out. Nothing here keeps a
//! timer: `next_wake` reads the deadline off the current `Link` variant, and
//! the app wakes the network then with `Input::Wake`.

use std::fmt;
use std::time::{Duration, Instant};

use crate::irc::isupport::Isupport;
use crate::irc::{Message, WireLine};
use crate::line::{Destination, Line, LineType};
use crate::model::buffer::{Body, BufferId, Buffers};
use crate::model::registration::{self, Failure, Progress, Registration, Setup, SetupError};
use crate::model::session::Session;
use crate::model::{Change, Cx};

const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(240);
/// Textual's `_pingInterval` and `_timeoutInterval`: silence for this long
/// draws a PING, then closes the link.
const IDLE_PING: Duration = Duration::from_secs(270);
const IDLE_CLOSE: Duration = Duration::from_secs(360);
/// How long to wait after QUIT for the server to close, so its ERROR
/// reply arrives.
const QUIT_GRACE: Duration = Duration::from_secs(5);
const BACKOFF_FIRST: Duration = Duration::from_secs(5);
const BACKOFF_CAP: Duration = Duration::from_secs(300);
/// A link that stayed online this long was not a flapping one, so the
/// next failure starts the backoff again.
const STABLE_ONLINE: Duration = Duration::from_secs(60);
/// Longer than the transport's own 30 s connect timeout.
const CONNECT_DEADLINE: Duration = Duration::from_secs(45);
const PING_TOKEN: &str = "textual-gtk";

/// Stable identity of a configured server; seeds the backoff jitter so two
/// networks that drop together do not retry in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServerId(pub u128);

/// One dial attempt. Every transport event carries the id of the attempt
/// it belongs to, so an event from an earlier attempt can be recognised and
/// dropped. Ids are unique within one `Network`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnId(u64);

impl ConnId {
    pub const fn new(n: u64) -> Self {
        ConnId(n)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ConnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub host: String,
    pub port: u16,
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tls {
    Off,
    /// `verify: false` is Textual's `validateServerCertificateChain = NO`:
    /// any certificate is accepted.
    On {
        verify: bool,
    },
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub id: ServerId,
    /// Tried in turn, one per attempt.
    pub addresses: Vec<Address>,
    pub tls: Tls,
    pub auto_reconnect: bool,
    pub setup: Setup,
    /// Normally `caps::WANTED`; `sasl` is added for `Auth::SaslPlain`.
    pub wanted_caps: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desired {
    Online,
    Offline,
}

#[derive(Debug)]
pub enum Input {
    /// The user wants this network online. Dials now unless a link is
    /// already up or on its way.
    Connect,
    /// The user wants this network offline: QUIT with this reason if the
    /// link is registered, hang up otherwise, and cancel any pending retry.
    Disconnect {
        reason: Option<String>,
    },
    Transport {
        conn: ConnId,
        event: TransportEvent,
    },
    /// The time from `next_wake` has come.
    Wake,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransportEvent {
    /// The socket is connected, and the TLS handshake done if there is one.
    Opened,
    /// A server line. The transport has already answered it if it was PING.
    Line(Message),
    /// Always the last event of a connection.
    Closed(CloseReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseReason {
    /// The server closed the connection.
    Eof,
    /// Dialing plus the TLS handshake took too long.
    ConnectTimeout,
    /// The certificate chain did not verify. `sha256` is the leaf
    /// certificate's fingerprint, for the trust prompt.
    BadCertificate {
        sha256: [u8; 32],
    },
    Error(String),
}

#[derive(Debug)]
pub enum Effect {
    Dial {
        conn: ConnId,
        address: Address,
        tls: Tls,
    },
    Send {
        conn: ConnId,
        line: WireLine,
    },
    /// Close the connection now. A no-op for a connection already gone.
    Hangup {
        conn: ConnId,
    },
}

/// What one input to a network produced.
#[derive(Debug, Default)]
pub(crate) struct Out {
    pub effects: Vec<Effect>,
    pub changes: Vec<Change>,
}

/// Something the console says about the connection.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    Connecting(Address),
    Registration(registration::Notice),
    Disconnected {
        why: Ended,
        retry_in: Option<Duration>,
    },
}

impl fmt::Display for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Notice::Connecting(address) => write!(f, "Connecting to {address}"),
            Notice::Registration(notice) => notice.fmt(f),
            Notice::Disconnected { why, retry_in } => {
                write!(f, "Disconnected: {why}")?;
                match retry_in {
                    Some(after) => write!(f, "; reconnecting in {} s", after.as_secs_f64().ceil()),
                    None => Ok(()),
                }
            }
        }
    }
}

/// Why a link ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Ended {
    UserQuit,
    Closed(CloseReason),
    /// The server sent ERROR.
    ServerError(String),
    /// 465 ERR_YOUREBANNEDCREEP.
    Banned(String),
    RegistrationTimedOut,
    /// Nothing arrived for `IDLE_CLOSE`, not even the answer to our PING.
    PingTimeout,
    NoUsableNick {
        tried: Vec<String>,
    },
    BadSetup(SetupError),
}

impl Ended {
    /// Whether trying again could help. Textual cancels the reconnect for
    /// the two flood ERRORs because reconnecting re-sends what flooded.
    fn retries(&self) -> bool {
        match self {
            Ended::UserQuit
            | Ended::Closed(CloseReason::BadCertificate { .. })
            | Ended::Banned(_)
            | Ended::BadSetup(_) => false,
            Ended::ServerError(text) => !(text.contains("Excess Flood") || text.contains("SendQ")),
            Ended::Closed(_)
            | Ended::RegistrationTimedOut
            | Ended::PingTimeout
            | Ended::NoUsableNick { .. } => true,
        }
    }
}

impl fmt::Display for Ended {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ended::UserQuit => f.write_str("quit"),
            Ended::Closed(CloseReason::Eof) => f.write_str("the server closed the connection"),
            Ended::Closed(CloseReason::ConnectTimeout) => f.write_str("the connection timed out"),
            Ended::Closed(CloseReason::BadCertificate { .. }) => {
                f.write_str("the server's certificate is not trusted")
            }
            Ended::Closed(CloseReason::Error(e)) => f.write_str(e),
            Ended::ServerError(text) => write!(f, "ERROR {text}"),
            Ended::Banned(text) => write!(f, "banned: {text}"),
            Ended::RegistrationTimedOut => f.write_str("registration timed out"),
            Ended::PingTimeout => f.write_str("no reply from the server"),
            Ended::NoUsableNick { tried } => {
                write!(f, "every nick was refused ({})", tried.join(", "))
            }
            Ended::BadSetup(e) => e.fmt(f),
        }
    }
}

/// The connection's state. `Registration` and `Session` live only in the
/// variants that have a socket, so ending a link cannot leave them behind.
// Registering holds the large Registration; boxing it would allocate on
// every dial for no gain, since there is one Link per network.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Link {
    Offline {
        last: Option<Ended>,
    },
    Waiting {
        until: Instant,
        last: Ended,
    },
    Connecting {
        conn: ConnId,
        address: Address,
        /// The transport times out first; this only ends a link whose
        /// transport task died without saying so.
        deadline: Instant,
    },
    Registering {
        conn: ConnId,
        reg: Registration,
        deadline: Instant,
    },
    Online {
        conn: ConnId,
        session: Session,
        since: Instant,
        idle: Idle,
    },
    Quitting {
        conn: ConnId,
        session: Session,
        until: Instant,
    },
}

/// Liveness of an online link. Any line from the server counts, so a busy
/// link is never pinged.
#[derive(Debug, Clone, Copy)]
pub struct Idle {
    pub last_heard: Instant,
    pub pinged: bool,
}

impl Idle {
    fn deadline(self) -> Instant {
        self.last_heard + if self.pinged { IDLE_CLOSE } else { IDLE_PING }
    }
}

#[derive(Debug)]
pub struct Network {
    config: ServerConfig,
    desired: Desired,
    link: Link,
    /// Consecutive failed links. Kept here, not in `Link`, because it has to
    /// outlive the live variants: a server that accepts and then drops us
    /// still backs off.
    failures: u32,
    next_address: usize,
    next_conn: u64,
    /// Outlive every link, so a reconnect finds its channels and queries.
    buffers: Buffers,
}

impl Network {
    pub(crate) fn new(config: ServerConfig, console: BufferId) -> Self {
        assert!(!config.addresses.is_empty(), "a server needs an address");
        let buffers = Buffers::new(console, &config.addresses[0].host);
        Network {
            config,
            desired: Desired::Offline,
            link: Link::Offline { last: None },
            failures: 0,
            next_address: 0,
            next_conn: 1,
            buffers,
        }
    }

    pub fn id(&self) -> ServerId {
        self.config.id
    }

    pub fn buffers(&self) -> &Buffers {
        &self.buffers
    }

    pub(crate) fn buffers_mut(&mut self) -> &mut Buffers {
        &mut self.buffers
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn desired(&self) -> Desired {
        self.desired
    }

    pub fn link(&self) -> &Link {
        &self.link
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    pub fn session(&self) -> Option<&Session> {
        match &self.link {
            Link::Online { session, .. } | Link::Quitting { session, .. } => Some(session),
            _ => None,
        }
    }

    /// When `update` next needs `Input::Wake`, if ever.
    pub fn next_wake(&self) -> Option<Instant> {
        match &self.link {
            Link::Offline { .. } => None,
            Link::Waiting { until, .. } | Link::Quitting { until, .. } => Some(*until),
            Link::Connecting { deadline, .. } | Link::Registering { deadline, .. } => {
                Some(*deadline)
            }
            Link::Online { idle, .. } => Some(idle.deadline()),
        }
    }

    pub(crate) fn update(&mut self, input: Input, cx: &mut Cx, out: &mut Out) {
        self.expire(cx, out);
        match input {
            Input::Connect => self.connect(cx, out),
            Input::Disconnect { reason } => self.disconnect(reason, cx, out),
            Input::Transport { conn, event } if Some(conn) == self.conn() => {
                self.on_transport(event, cx, out);
            }
            Input::Transport { .. } | Input::Wake => {}
        }
    }

    fn conn(&self) -> Option<ConnId> {
        match &self.link {
            Link::Offline { .. } | Link::Waiting { .. } => None,
            Link::Connecting { conn, .. }
            | Link::Registering { conn, .. }
            | Link::Online { conn, .. }
            | Link::Quitting { conn, .. } => Some(*conn),
        }
    }

    fn connect(&mut self, cx: &mut Cx, out: &mut Out) {
        self.desired = Desired::Online;
        match self.link {
            Link::Offline { .. } => {
                self.failures = 0;
                self.dial(cx, out);
            }
            Link::Waiting { .. } => self.dial(cx, out),
            // A connect while quitting dials once the quit is done.
            _ => {}
        }
    }

    fn disconnect(&mut self, reason: Option<String>, cx: &mut Cx, out: &mut Out) {
        self.desired = Desired::Offline;
        match std::mem::replace(&mut self.link, Link::Offline { last: None }) {
            Link::Online {
                conn,
                session,
                since,
                ..
            } => {
                // The link ends from Quitting, where `end` cannot see how
                // long it was online, so a stable session resets here.
                if cx.now.mono.duration_since(since) >= STABLE_ONLINE {
                    self.failures = 0;
                }
                out.effects.push(Effect::Send {
                    conn,
                    line: quit_line(reason),
                });
                self.link = Link::Quitting {
                    conn,
                    session,
                    until: cx.now.mono + QUIT_GRACE,
                };
            }
            Link::Connecting { conn, .. } | Link::Registering { conn, .. } => {
                out.effects.push(Effect::Hangup { conn });
                self.end(Ended::UserQuit, cx, out);
            }
            Link::Waiting { .. } => {
                self.link = Link::Offline {
                    last: Some(Ended::UserQuit),
                };
            }
            other => self.link = other,
        }
    }

    fn dial(&mut self, cx: &mut Cx, out: &mut Out) {
        let addresses = &self.config.addresses;
        let address = addresses[self.next_address].clone();
        self.next_address = (self.next_address + 1) % addresses.len();
        let conn = ConnId(self.next_conn);
        self.next_conn += 1;
        self.notice(Notice::Connecting(address.clone()), cx, out);
        out.effects.push(Effect::Dial {
            conn,
            address: address.clone(),
            tls: self.config.tls,
        });
        self.link = Link::Connecting {
            conn,
            address,
            deadline: cx.now.mono + CONNECT_DEADLINE,
        };
    }

    fn expire(&mut self, cx: &mut Cx, out: &mut Out) {
        if self.next_wake().is_none_or(|at| cx.now.mono < at) {
            return;
        }
        match &mut self.link {
            Link::Waiting { .. } => self.dial(cx, out),
            Link::Connecting { conn, .. } => {
                out.effects.push(Effect::Hangup { conn: *conn });
                self.end(Ended::Closed(CloseReason::ConnectTimeout), cx, out);
            }
            Link::Registering { conn, .. } => {
                out.effects.push(Effect::Hangup { conn: *conn });
                self.end(Ended::RegistrationTimedOut, cx, out);
            }
            Link::Online { conn, idle, .. } if !idle.pinged => {
                idle.pinged = true;
                let line = Message::new("PING", &[PING_TOKEN]).to_wire();
                out.effects.push(Effect::Send {
                    conn: *conn,
                    line: line.expect("a fixed, valid line"),
                });
            }
            Link::Online { conn, .. } => {
                out.effects.push(Effect::Hangup { conn: *conn });
                self.end(Ended::PingTimeout, cx, out);
            }
            Link::Quitting { conn, .. } => {
                out.effects.push(Effect::Hangup { conn: *conn });
                self.end(Ended::UserQuit, cx, out);
            }
            Link::Offline { .. } => {}
        }
    }

    fn on_transport(&mut self, event: TransportEvent, cx: &mut Cx, out: &mut Out) {
        match event {
            TransportEvent::Opened => self.on_opened(cx, out),
            TransportEvent::Line(msg) => self.on_line(&msg, cx, out),
            TransportEvent::Closed(reason) => {
                let why = match self.link {
                    Link::Quitting { .. } => Ended::UserQuit,
                    _ => Ended::Closed(reason),
                };
                self.end(why, cx, out);
            }
        }
    }

    fn on_opened(&mut self, cx: &mut Cx, out: &mut Out) {
        let Link::Connecting { conn, .. } = self.link else {
            return;
        };
        let mut reg_out = registration::Output::default();
        let wanted: Vec<&str> = self.config.wanted_caps.iter().map(String::as_str).collect();
        match Registration::start(self.config.setup.clone(), &wanted, &mut reg_out) {
            Ok(reg) => {
                self.link = Link::Registering {
                    conn,
                    reg,
                    deadline: cx.now.mono + REGISTRATION_TIMEOUT,
                };
                self.push_output(conn, reg_out, cx, out);
            }
            Err(e) => {
                out.effects.push(Effect::Hangup { conn });
                self.end(Ended::BadSetup(e), cx, out);
            }
        }
    }

    fn on_line(&mut self, msg: &Message, cx: &mut Cx, out: &mut Out) {
        let Some(conn) = self.conn() else { return };
        let text = || msg.params.last().cloned().unwrap_or_default();
        match (msg.command.as_str(), &mut self.link) {
            ("465", _) => {
                out.effects.push(Effect::Hangup { conn });
                self.end(Ended::Banned(text()), cx, out);
            }
            ("ERROR", Link::Online { .. }) => {
                out.effects.push(Effect::Hangup { conn });
                self.end(Ended::ServerError(text()), cx, out);
            }
            ("ERROR", Link::Quitting { .. }) => {
                out.effects.push(Effect::Hangup { conn });
                self.end(Ended::UserQuit, cx, out);
            }
            (_, Link::Online { session, idle, .. }) => {
                *idle = Idle {
                    last_heard: cx.now.mono,
                    pinged: false,
                };
                session.handle(msg, conn, &mut self.buffers, cx, out);
            }
            (_, Link::Quitting { session, .. }) => {
                session.handle(msg, conn, &mut self.buffers, cx, out);
            }
            (_, Link::Registering { .. }) => self.step_registration(msg, cx, out),
            (_, Link::Offline { .. } | Link::Waiting { .. } | Link::Connecting { .. }) => {}
        }
    }

    fn step_registration(&mut self, msg: &Message, cx: &mut Cx, out: &mut Out) {
        let Link::Registering {
            conn,
            reg,
            deadline,
        } = std::mem::replace(&mut self.link, Link::Offline { last: None })
        else {
            unreachable!("only called while registering");
        };
        let nick = reg.nick().to_owned();
        let mut reg_out = registration::Output::default();
        let progress = reg.step(msg, &mut reg_out);
        self.print_registration_line(msg, &nick, cx, out);
        self.push_output(conn, reg_out, cx, out);
        match progress {
            Progress::Registering(reg) => {
                self.link = Link::Registering {
                    conn,
                    reg,
                    deadline,
                };
            }
            Progress::Registered(registered) => {
                self.link = Link::Online {
                    conn,
                    session: Session::new(registered),
                    since: cx.now.mono,
                    idle: Idle {
                        last_heard: cx.now.mono,
                        pinged: false,
                    },
                };
            }
            Progress::Failed(failure) => {
                out.effects.push(Effect::Hangup { conn });
                let why = match failure {
                    Failure::NoUsableNick { tried } => Ended::NoUsableNick { tried },
                    Failure::ServerError(text) => Ended::ServerError(text),
                };
                self.end(why, cx, out);
            }
        }
    }

    fn push_output(&mut self, conn: ConnId, reg: registration::Output, cx: &mut Cx, out: &mut Out) {
        out.effects.extend(
            reg.lines
                .into_iter()
                .map(|line| Effect::Send { conn, line }),
        );
        for notice in reg.notices {
            self.notice(Notice::Registration(notice), cx, out);
        }
    }

    /// Prints a connection notice to the console.
    fn notice(&mut self, notice: Notice, cx: &mut Cx, out: &mut Out) {
        let line = Line::client(cx.stamp(), notice.to_string());
        let console = self.buffers.console().id();
        self.buffers.print(console, line, &mut out.changes);
    }

    /// What a server says while we register goes to the console, as in
    /// Textual, except what registration explains itself: CAP, SASL and
    /// the nick numerics.
    fn print_registration_line(&mut self, msg: &Message, nick: &str, cx: &mut Cx, out: &mut Out) {
        if matches!(
            msg.command.as_str(),
            "CAP"
                | "AUTHENTICATE"
                | "432"
                | "433"
                | "437"
                | "900"
                | "901"
                | "902"
                | "903"
                | "904"
                | "905"
                | "906"
                | "907"
                | "908"
        ) {
            return;
        }
        if let Some((Destination::Server, line)) =
            Line::from_message(msg, nick, &Isupport::default(), cx.stamp())
        {
            let console = self.buffers.console().id();
            self.buffers.print(console, line, &mut out.changes);
        }
    }

    /// Sends a message typed into `buffer`. Without echo-message it is
    /// printed here; with it, the server's echo prints it.
    pub(crate) fn say(&mut self, buffer: BufferId, text: &str, cx: &mut Cx, out: &mut Out) {
        let Some(target) = self.buffers.get(buffer).map(|b| b.name().to_owned()) else {
            return;
        };
        let refusal = match (&self.link, self.buffers.get(buffer)) {
            (_, Some(b)) if b.is_console() => Some("Messages cannot be sent to this window."),
            (Link::Online { .. }, Some(b))
                if b.joined().is_some() || matches!(b.body(), Body::Query) =>
            {
                None
            }
            (Link::Online { .. }, _) => Some("You are not in this channel."),
            _ => Some("Not connected."),
        };
        if let Some(text) = refusal {
            let line = Line::client(cx.stamp(), text.to_owned());
            self.buffers.print(buffer, line, &mut out.changes);
            return;
        }
        let Link::Online { conn, session, .. } = &self.link else {
            return;
        };
        let line = match Message::new("PRIVMSG", &[&target, text]).to_wire() {
            Ok(line) => line,
            Err(e) => {
                let line = Line::client(cx.stamp(), format!("Not sent: {e}"));
                self.buffers.print(buffer, line, &mut out.changes);
                return;
            }
        };
        out.effects.push(Effect::Send { conn: *conn, line });
        if !session.caps().is_enabled("echo-message") {
            let mut line = Line::new(
                cx.stamp(),
                LineType::Privmsg,
                "PRIVMSG",
                Some(session.nick()),
                text.to_owned(),
            );
            line.from_self = true;
            self.buffers.print(buffer, line, &mut out.changes);
        }
    }

    /// Closes a channel or query buffer, leaving the channel if we are in
    /// it. The console stays.
    pub(crate) fn close(&mut self, buffer: BufferId, out: &mut Out) {
        let Some(b) = self.buffers.get(buffer) else {
            return;
        };
        if b.is_console() {
            return;
        }
        if b.joined().is_some()
            && let Link::Online { conn, .. } = &self.link
            && let Ok(line) = Message::new("PART", &[b.name()]).to_wire()
        {
            out.effects.push(Effect::Send { conn: *conn, line });
        }
        self.buffers.remove(buffer, &mut out.changes);
    }

    /// Ends the current link (the caller has already hung up if a socket
    /// was open) and decides what comes next.
    fn end(&mut self, why: Ended, cx: &mut Cx, out: &mut Out) {
        self.buffers.part_all(&mut out.changes);
        if let Link::Online { since, .. } = self.link
            && cx.now.mono.duration_since(since) >= STABLE_ONLINE
        {
            self.failures = 0;
        }
        if why != Ended::UserQuit {
            self.failures = self.failures.saturating_add(1);
        }
        let retry = self.desired == Desired::Online && self.config.auto_reconnect && why.retries();
        let retry_in = retry.then(|| backoff(self.config.id, self.failures));
        self.notice(
            Notice::Disconnected {
                why: why.clone(),
                retry_in,
            },
            cx,
            out,
        );
        match retry_in {
            Some(after) => {
                self.link = Link::Waiting {
                    until: cx.now.mono + after,
                    last: why,
                };
            }
            // `/reconnect` is a quit with the network still wanted online.
            None if why == Ended::UserQuit && self.desired == Desired::Online => self.dial(cx, out),
            None => self.link = Link::Offline { last: Some(why) },
        }
    }
}

/// 5 s doubling to 300 s, less up to a tenth taken from a hash of
/// (server, failures), so the sequence is the same in every run.
fn backoff(server: ServerId, failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    let base = (BACKOFF_FIRST * (1 << doublings)).min(BACKOFF_CAP);
    let base_ms = base.as_millis() as u64;
    let seed = (server.0 as u64) ^ ((server.0 >> 64) as u64) ^ u64::from(failures);
    let jitter_ms = splitmix64(seed) % (base_ms / 10);
    Duration::from_millis(base_ms - jitter_ms)
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// A reason that cannot go on the wire falls back to a bare QUIT, so the
/// server still sees a clean quit.
fn quit_line(reason: Option<String>) -> WireLine {
    reason
        .and_then(|r| Message::new("QUIT", &[&r]).to_wire().ok())
        .unwrap_or_else(|| Message::new("QUIT", &[]).to_wire().expect("a bare QUIT"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_the_cap_and_jitter_only_shortens() {
        for id in [
            0u128,
            1,
            u128::MAX,
            0x1234_5678_9abc_def0_1234_5678_9abc_def0,
        ] {
            let mut previous = Duration::ZERO;
            for failures in 1..=20 {
                let base = (BACKOFF_FIRST * (1 << (failures - 1).min(16))).min(BACKOFF_CAP);
                let got = backoff(ServerId(id), failures);
                assert!(got <= base && got > base * 9 / 10, "{failures}: {got:?}");
                if base < BACKOFF_CAP {
                    assert!(got > previous, "{failures}: {got:?} after {previous:?}");
                }
                previous = got;
            }
        }
        assert_eq!(backoff(ServerId(7), 3), backoff(ServerId(7), 3));
        assert_ne!(backoff(ServerId(7), 3), backoff(ServerId(8), 3));
    }
}
