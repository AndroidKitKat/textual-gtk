//! One configured server and the lifecycle of its connection: dial,
//! register, stay online, end, and decide whether and when to try again.
//!
//! `Network::update` is the only way in. It takes an `Input` and the current
//! time and pushes `Effect`s for the app to carry out. Nothing here keeps a
//! timer: `next_wake` reads the deadline off the current `Link` variant, and
//! the app wakes the network then with `Input::Wake`.

use std::fmt;
use std::time::{Duration, Instant};

use crate::input::split::{budget, split_for_wire};
use crate::irc::casemap::CaseMapping;
use crate::irc::isupport::Isupport;
use crate::irc::join::join_lines;
use crate::irc::{Message, WireLine};
use crate::line::{Destination, Line, LineType};
use crate::model::buffer::{BufferId, Buffers, Rejoin};
use crate::model::registration::{self, Failure, Progress, Registration, Setup, SetupError};
use crate::model::session::Session;
use crate::model::{Change, Cx, Ids, LEAVING_COMMENT};

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
/// Textual's `IRC[6rj-2r]`.
pub(crate) const NOT_CONNECTED: &str = "Failed to send data to server. Not connected.";

/// How a message goes out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speech {
    Privmsg,
    /// `/me`: a PRIVMSG wrapped in CTCP ACTION.
    Action,
    Notice,
}

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

/// A channel the network joins once it is online.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelConfig {
    pub name: String,
    pub key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub id: ServerId,
    /// What the sidebar calls this network.
    pub name: String,
    /// Tried in turn, one per attempt.
    pub addresses: Vec<Address>,
    pub tls: Tls,
    pub auto_reconnect: bool,
    pub setup: Setup,
    /// Shown in the sidebar while offline and joined after 001, keyed and
    /// unkeyed JOINs on separate lines.
    pub autojoin: Vec<ChannelConfig>,
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
    /// Textual's `temporaryServerAddressOverride`, set by `/conn host`: the
    /// next dial goes there, on 6667 without TLS, and only that one.
    address_override: Option<String>,
    next_conn: u64,
    /// Outlive every link, so a reconnect finds its channels and queries.
    buffers: Buffers,
}

impl Network {
    /// Creates the console and a parted buffer for each configured autojoin
    /// channel, announcing them in `changes`.
    pub(crate) fn new(config: ServerConfig, ids: &mut Ids, changes: &mut Vec<Change>) -> Self {
        assert!(!config.addresses.is_empty(), "a server needs an address");
        let console = ids.buffer();
        let mut buffers = Buffers::new(console, &config.addresses[0].host);
        changes.push(Change::BufferAdded(console));
        for channel in &config.autojoin {
            if buffers
                .find_channel(CaseMapping::default(), &channel.name)
                .is_none()
            {
                buffers.add_channel(ids.buffer(), &channel.name, Rejoin::OnConnect, changes);
            }
        }
        Network {
            config,
            desired: Desired::Offline,
            link: Link::Offline { last: None },
            failures: 0,
            next_address: 0,
            address_override: None,
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
        [self.link_deadline(), self.buffers.next_rejoin()]
            .into_iter()
            .flatten()
            .min()
    }

    fn link_deadline(&self) -> Option<Instant> {
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
        let before = std::mem::discriminant(&self.link);
        self.expire(cx, out);
        match input {
            Input::Connect => self.connect(cx, out),
            Input::Disconnect { reason } => self.disconnect(reason, cx, out),
            Input::Transport { conn, event } if Some(conn) == self.conn() => {
                self.on_transport(event, cx, out);
            }
            Input::Transport { .. } | Input::Wake => {}
        }
        // Announced here, once, so no transition can forget it.
        if std::mem::discriminant(&self.link) != before {
            out.changes.push(Change::Link(self.config.id));
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
        let (address, tls) = match self.address_override.take() {
            Some(host) => (Address { host, port: 6667 }, Tls::Off),
            None => {
                let addresses = &self.config.addresses;
                let address = addresses[self.next_address].clone();
                self.next_address = (self.next_address + 1) % addresses.len();
                (address, self.config.tls)
            }
        };
        let conn = ConnId(self.next_conn);
        self.next_conn += 1;
        self.notice(Notice::Connecting(address.clone()), cx, out);
        out.effects.push(Effect::Dial {
            conn,
            address: address.clone(),
            tls,
        });
        self.link = Link::Connecting {
            conn,
            address,
            deadline: cx.now.mono + CONNECT_DEADLINE,
        };
    }

    fn expire(&mut self, cx: &mut Cx, out: &mut Out) {
        if self.link_deadline().is_some_and(|at| cx.now.mono >= at) {
            self.expire_link(cx, out);
        }
        let due = self.buffers.take_due_rejoins(cx.now.mono);
        self.send_joins(&due, out);
    }

    fn expire_link(&mut self, cx: &mut Cx, out: &mut Out) {
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
        let explained = reg.is_ladder_rejection(msg);
        let mut reg_out = registration::Output::default();
        let progress = reg.step(msg, &mut reg_out);
        if !explained {
            self.print_registration_line(msg, &nick, cx, out);
        }
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
                self.join_waiting(out);
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

    /// After 001: the channels that were open when the last link ended, and
    /// the configured ones even if the user left them.
    fn join_waiting(&self, out: &mut Out) {
        let casemap = self
            .session()
            .map_or_else(CaseMapping::default, Session::casemap);
        let waiting: Vec<BufferId> = self
            .buffers
            .iter()
            .filter(|b| match b.rejoin() {
                Some(Rejoin::OnConnect) => true,
                Some(Rejoin::No) => self.configured(casemap, b.name()).is_some(),
                Some(Rejoin::At(_)) | None => false,
            })
            .map(|b| b.id())
            .collect();
        self.send_joins(&waiting, out);
    }

    fn configured(&self, casemap: CaseMapping, name: &str) -> Option<&ChannelConfig> {
        let key = casemap.chan(name);
        self.config
            .autojoin
            .iter()
            .find(|c| casemap.chan(&c.name) == key)
    }

    /// Sends the JOINs for these channels, each with its configured key.
    fn send_joins(&self, channels: &[BufferId], out: &mut Out) {
        let Link::Online { conn, session, .. } = &self.link else {
            return;
        };
        let casemap = session.casemap();
        let entries: Vec<(&str, Option<&str>)> = channels
            .iter()
            .filter_map(|id| self.buffers.get(*id))
            .map(|b| {
                let key = self.configured(casemap, b.name());
                (b.name(), key.and_then(|c| c.key.as_deref()))
            })
            .collect();
        out.effects.extend(
            join_lines(&entries)
                .into_iter()
                .map(|line| Effect::Send { conn: *conn, line }),
        );
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
    /// the refusals of the nick ladder's nicks.
    fn print_registration_line(&mut self, msg: &Message, nick: &str, cx: &mut Cx, out: &mut Out) {
        if matches!(
            msg.command.as_str(),
            "CAP"
                | "AUTHENTICATE"
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

    /// Textual's `isLoggedIn`: registered.
    pub fn is_logged_in(&self) -> bool {
        matches!(self.link, Link::Online { .. })
    }

    /// Textual's `isConnected`: a socket is up, registered or not.
    pub fn is_connected(&self) -> bool {
        matches!(
            self.link,
            Link::Registering { .. } | Link::Online { .. } | Link::Quitting { .. }
        )
    }

    /// Where the next dial goes instead of the configured addresses.
    pub(crate) fn override_address(&mut self, host: String) {
        self.address_override = Some(host);
    }

    /// Our nick: the session's, or the configured one before 001.
    pub fn nick(&self) -> &str {
        self.session()
            .map_or(self.config.setup.nick.as_str(), Session::nick)
    }

    pub fn casemap(&self) -> CaseMapping {
        self.session()
            .map_or_else(CaseMapping::default, Session::casemap)
    }

    /// Textual's `sendLine:`: without a socket the line is dropped and the
    /// console says so (`IRCClient.m:2659-2674`).
    pub(crate) fn send_line(&mut self, line: WireLine, cx: &mut Cx, out: &mut Out) {
        match self.conn().filter(|_| self.is_connected()) {
            Some(conn) => out.effects.push(Effect::Send { conn, line }),
            None => {
                let console = self.buffers.console().id();
                self.print_client(console, NOT_CONNECTED, cx, out);
            }
        }
    }

    /// Prints a line the client says itself into `buffer`.
    pub(crate) fn print_client(
        &mut self,
        buffer: BufferId,
        text: &str,
        cx: &mut Cx,
        out: &mut Out,
    ) {
        let line = Line::client(cx.stamp(), text.to_owned());
        self.buffers.print(buffer, line, &mut out.changes);
    }

    /// Textual's `sendText:asCommand:toChannel:` and the send half of
    /// `/msg`: cuts `text` to fit the wire, sends each part to `target`,
    /// and prints it into `echo` unless the server echoes our messages.
    pub(crate) fn send_text(
        &mut self,
        target: &str,
        speech: Speech,
        text: &str,
        echo: Option<BufferId>,
        cx: &mut Cx,
        out: &mut Out,
    ) {
        let (command, kind) = match speech {
            Speech::Privmsg => ("PRIVMSG", LineType::Privmsg),
            Speech::Action => ("PRIVMSG", LineType::Action),
            Speech::Notice => ("NOTICE", LineType::Notice),
        };
        let nick = self.nick().to_owned();
        let user_host = self
            .session()
            .and_then(Session::hostmask)
            .map(str::to_owned);
        let echoed = self
            .session()
            .is_some_and(|s| s.caps().is_enabled("echo-message"));
        let casemap = self.casemap();
        let room = budget(
            &nick,
            user_host.as_deref(),
            command,
            target,
            speech == Speech::Action,
        );
        for part in split_for_wire(text, room) {
            if !echoed && let Some(buffer) = echo {
                let mut line = Line::new(cx.stamp(), kind, command, Some(&nick), part.clone());
                line.from_self = true;
                let now = cx.now.mono;
                self.buffers
                    .converse(buffer, &line, &nick, casemap, false, now);
                self.buffers.print(buffer, line, &mut out.changes);
            }
            let body = match speech {
                Speech::Action => format!("\x01ACTION {part}\x01"),
                _ => part,
            };
            match Message::new(command, &[target, &body]).to_wire() {
                Ok(line) => self.send_line(line, cx, out),
                Err(e) => {
                    let console = self.buffers.console().id();
                    self.print_client(console, &format!("Not sent: {e}"), cx, out);
                }
            }
        }
    }

    /// The query with `nick`, opened if there is none.
    pub(crate) fn open_query(&mut self, nick: &str, ids: &mut Ids, out: &mut Out) -> BufferId {
        match self.buffers.find_query(self.casemap(), nick) {
            Some(id) => id,
            None => self.buffers.add_query(ids.buffer(), nick, &mut out.changes),
        }
    }

    /// The buffer of channel `name`, added parted if there is none, as
    /// Textual's `findChannelOrCreate` does before a JOIN goes out.
    pub(crate) fn open_channel(&mut self, name: &str, ids: &mut Ids, out: &mut Out) -> BufferId {
        match self.buffers.find_channel(self.casemap(), name) {
            Some(id) => id,
            None => self
                .buffers
                .add_channel(ids.buffer(), name, Rejoin::No, &mut out.changes),
        }
    }

    /// Closes a channel or query buffer, leaving the channel with the
    /// leaving comment if we are in it (Textual's `destroyChannel:`). The
    /// console stays.
    pub(crate) fn close(&mut self, buffer: BufferId, out: &mut Out) {
        let Some(b) = self.buffers.get(buffer) else {
            return;
        };
        if b.is_console() {
            return;
        }
        if b.joined().is_some()
            && let Link::Online { conn, .. } = &self.link
            && let Ok(line) = Message::new("PART", &[b.name(), LEAVING_COMMENT]).to_wire()
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
            // `/conn` is a quit with the network still wanted online.
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
