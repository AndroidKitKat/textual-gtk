//! Candidate 1: "one sans-IO world on the GTK thread".
//!
//! Every piece of client state (config, links, sessions, buffers, members,
//! unread, focus) lives in one `World` value, owned by the GTK main thread and
//! changed only by `World::apply`. Sockets are dumb transports on tokio that
//! move parsed `Message`s in and wire lines out. The GTK shell is a projection:
//! it reads `&World` after each `apply` and patches its ListModels and chat
//! views from the returned `Change`s, then executes the returned `Effect`s.
//!
//! This file is std-only so it type-checks without GTK. Module boundaries are
//! marked with `mod` blocks that map 1:1 to the files in design.md.
#![allow(dead_code, unused_variables)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Stand-ins for existing crate items (src/irc/message.rs, src/line.rs).
// ---------------------------------------------------------------------------

/// `crate::irc::Message`, unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Message {
    pub tags: Vec<(String, String)>,
    pub source: Option<String>,
    pub command: String,
    pub params: Vec<String>,
}

/// `crate::line::Line`, unchanged except that `from_message` becomes
/// `line::describe` (below) and routing moves into the session.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub kind: LineType,
    pub nickname: Option<String>,
    pub from_self: bool,
    pub body: String,
    pub highlight: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineType {
    Privmsg,
    Action,
    Notice,
    Join,
    Part,
    Quit,
    Kick,
    Nick,
    Mode,
    Topic,
    Debug,
}

// ---------------------------------------------------------------------------
// src/irc/casemap.rs
// ---------------------------------------------------------------------------
pub mod casemap {
    /// ISUPPORT CASEMAPPING. Default before 005 is rfc1459 (RFC 2812 §2.2).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum Casemapping {
        Ascii,
        #[default]
        Rfc1459,
        Rfc1459Strict,
    }

    impl Casemapping {
        /// Unknown values (e.g. `rfc7613`) fall back to rfc1459 and are noted
        /// in the console, rather than silently using ascii.
        pub fn from_isupport(value: &str) -> Self {
            todo!()
        }

        /// The only constructor of `Key`. ascii: A-Z→a-z. rfc1459 adds
        /// `[]\~`→`{}|^`; strict omits `~`→`^`.
        pub fn fold(self, name: &str) -> Key {
            todo!()
        }
    }

    /// A casefolded nick or channel name. Every map keyed by a nick or a
    /// channel uses `Key`, so a lookup with a raw `&str` does not compile.
    /// A `Key` is meaningful only inside the `Session` whose casemapping made
    /// it; sessions never exchange keys.
    #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
    pub struct Key(String);

    /// A name as the server spelled it, plus its key. Display uses `.shown`;
    /// identity uses `.key`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Name {
        pub shown: String,
        pub key: Key,
    }
}
use casemap::{Casemapping, Key, Name};

// ---------------------------------------------------------------------------
// src/irc/isupport.rs
// ---------------------------------------------------------------------------
pub mod isupport {
    use super::casemap::Casemapping;

    /// Parsed 005 state. Tokens merge additively; `-TOKEN` restores the default.
    #[derive(Debug, Clone)]
    pub struct Isupport {
        pub casemapping: Casemapping,
        /// PREFIX in rank order, highest first: `[('o','@'), ('v','+')]`.
        /// A member's modes are a bitset over this index (see `members`).
        pub prefix: Vec<(char, char)>,
        pub chantypes: String,
        /// CHANMODES groups A, B, C, D.
        pub chanmodes: [String; 4],
        pub statusmsg: String,
        pub network: Option<String>,
        pub nicklen: usize,
        pub whox: bool,
    }

    impl Default for Isupport {
        /// `#&`, `(ov)@+`, rfc1459, NICKLEN 9.
        fn default() -> Self {
            todo!()
        }
    }

    /// What a 005 line changed that the session must react to.
    pub struct IsupportDelta {
        pub casemapping_changed: bool,
        pub prefix_changed: bool,
    }

    impl Isupport {
        /// `params` without the leading nick and the trailing text.
        pub fn merge(&mut self, params: &[String]) -> IsupportDelta {
            todo!()
        }
        pub fn is_channel(&self, target: &str) -> bool {
            todo!()
        }
        /// Index into `prefix` for a mode letter or a symbol.
        pub fn rank_of_mode(&self, mode: char) -> Option<u8> {
            todo!()
        }
        pub fn rank_of_symbol(&self, symbol: char) -> Option<u8> {
            todo!()
        }
        /// Whether a channel mode letter takes a parameter when set/unset.
        pub fn takes_param(&self, mode: char, adding: bool) -> bool {
            todo!()
        }
    }
}
use isupport::Isupport;

// ---------------------------------------------------------------------------
// src/config/mod.rs  (TOML, serde; this sketch omits the derives)
// ---------------------------------------------------------------------------
pub mod config {
    use super::*;

    /// Stable ids, generated once and persisted. Also the libsecret keys.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
    pub struct ServerId(pub u128);
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ChannelId(pub u128);
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct AddressId(pub u128);

    /// `~/.config/textual-gtk/config.toml`. Written only on user edits
    /// (add/remove server, /join adds a channel, /close removes it).
    #[derive(Debug, Clone)]
    pub struct Config {
        pub version: u32,
        pub servers: Vec<ServerConfig>,
    }

    #[derive(Debug, Clone)]
    pub struct ServerConfig {
        pub id: ServerId,
        pub name: String,
        /// Non-empty (enforced by `load`). Tried round-robin.
        pub addresses: Vec<Address>,
        pub nickname: String,
        pub alternate_nicknames: Vec<String>,
        pub username: String,
        pub realname: String,
        pub auth: Auth,
        pub auto_connect: bool,
        pub auto_reconnect: bool,
        pub autojoin_waits_for_identify: bool,
        pub on_connect_commands: Vec<String>,
        pub flood_control: FloodControl,
        pub channels: Vec<ChannelConfig>,
        pub highlight_words: Vec<String>,
    }

    #[derive(Debug, Clone)]
    pub struct Address {
        pub id: AddressId,
        pub host: String,
        pub port: u16,
        pub tls: Tls,
        /// Server password lives in libsecret under this address id.
        pub has_password: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Tls {
        Off,
        /// rustls with the platform verifier.
        Verified,
        /// Textual's `validateServerCertificateChain = NO`.
        Unverified,
    }

    /// Explicit, unlike Textual, which infers SASL PLAIN from "a NickServ
    /// password is set". Migration maps that inference onto this enum.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Auth {
        None,
        /// Account defaults to `username` (Textual's authcid).
        SaslPlain { account: Option<String> },
        /// Client certificates land later; the variant exists so config
        /// written now needs no migration then.
        SaslExternal,
        /// `PRIVMSG NickServ :IDENTIFY <pw>` after 001.
        NickServ,
    }

    #[derive(Debug, Clone, Copy)]
    pub struct FloodControl {
        pub lines: u32,
        pub per: Duration,
    }

    #[derive(Debug, Clone)]
    pub struct ChannelConfig {
        pub id: ChannelId,
        pub name: String,
        pub autojoin: bool,
        /// JOIN key in libsecret under `id`.
        pub has_key: bool,
    }

    /// `~/.local/state/textual-gtk/state.toml`: runtime state the app owns
    /// and the user never edits. Saved at quit and every 300 s when dirty.
    #[derive(Debug, Clone, Default)]
    pub struct UiState {
        pub expanded: HashSet<ServerId>,
        pub selection: Option<SavedSelection>,
        pub member_list_visible: bool,
        pub window_size: Option<(i32, i32)>,
        /// ZNC playback hook (phase 3): the last server-time seen per server.
        pub last_server_time: HashMap<ServerId, String>,
    }

    #[derive(Debug, Clone)]
    pub enum SavedSelection {
        Console(ServerId),
        Channel(ServerId, ChannelId),
    }

    #[derive(Debug)]
    pub enum ConfigError {
        Io(std::io::Error),
        /// File, line, message: shown in a dialog, the file is left alone.
        Syntax(String),
        NewerVersion(u32),
    }

    /// Parse, then run `migrate` up to `CURRENT`. A missing file is an
    /// empty config, not an error.
    pub fn load(path: &std::path::Path) -> Result<Config, ConfigError> {
        todo!()
    }
    /// Atomic: write `config.toml.tmp`, fsync, rename.
    pub fn save(path: &std::path::Path, config: &Config) -> std::io::Result<()> {
        todo!()
    }
    pub const CURRENT: u32 = 1;

    /// What an edit means for a live link. Pure; `World` applies it.
    #[derive(Debug, PartialEq)]
    pub enum LiveChange {
        /// Takes effect at the next connect (addresses, TLS, auth, username).
        NextConnect,
        ChannelAdded(ChannelId),
        ChannelRemoved(ChannelId),
        Renamed,
    }
    pub fn live_changes(old: &ServerConfig, new: &ServerConfig) -> Vec<LiveChange> {
        todo!()
    }
}
use config::{ChannelId, Config, ServerConfig, ServerId, UiState};

// ---------------------------------------------------------------------------
// src/config/secrets.rs  (oo7; async, runs on tokio, never on the GTK thread)
// ---------------------------------------------------------------------------
pub mod secrets {
    use super::config::{AddressId, ChannelId, ServerId};

    /// Attribute set `{app: "io.github.androidkitkat.TextualGtk", kind, id}`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum SecretKey {
        ServerPassword(AddressId),
        /// SASL PLAIN or NickServ password (Textual's `nicknamePassword`).
        Account(ServerId),
        ChannelKey(ChannelId),
    }

    /// A secret value. `Debug` prints `Secret(..)`.
    #[derive(Clone)]
    pub struct Secret(String);
    impl std::fmt::Debug for Secret {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Secret(..)")
        }
    }
    impl Secret {
        pub fn expose(&self) -> &str {
            &self.0
        }
    }

    /// What one connection attempt needs, fetched by the transport before
    /// it dials and handed to the session in `TransportEvent::Connected`.
    /// The session drops it after registration; `World` never stores it.
    #[derive(Debug, Clone, Default)]
    pub struct Secrets {
        pub server_password: Option<Secret>,
        pub account_password: Option<Secret>,
        pub channel_keys: Vec<(ChannelId, Secret)>,
    }

    // async fn load(keys: &[SecretKey]) -> Result<Secrets, oo7::Error>
    // async fn store(key: SecretKey, value: Secret) -> Result<(), oo7::Error>
    // async fn delete(key: SecretKey) -> Result<(), oo7::Error>
}
use secrets::Secrets;

// ---------------------------------------------------------------------------
// src/irc/transport.rs  (was connection.rs; tokio side, sketched as types)
// ---------------------------------------------------------------------------
pub mod transport {
    use super::*;

    /// Increments on every connect attempt of a server. Events, timers and
    /// effects carry it; `World` drops anything from an older epoch. That is
    /// what makes late events and never-cancelled timers harmless.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
    pub struct Epoch(pub u64);

    /// What the transport needs to dial. Built by `World` from config.
    #[derive(Debug, Clone)]
    pub struct Dial {
        pub host: String,
        pub port: u16,
        pub tls: config::Tls,
        pub secrets: Vec<secrets::SecretKey>,
        pub flood_control: config::FloodControl,
    }

    #[derive(Debug)]
    pub enum TransportEvent {
        /// TCP (and TLS) is up and secrets are loaded.
        Connected { tls: Option<TlsInfo>, secrets: Secrets },
        /// One parsed line. PING never arrives here: the transport answers it.
        Line(Message),
        /// Always last.
        Closed(LinkError),
    }

    #[derive(Debug, Clone)]
    pub struct TlsInfo {
        pub protocol: String,
        pub cipher: String,
        pub peer_sha256: String,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub enum LinkError {
        /// We sent QUIT (or asked to close) and the link ended.
        Requested,
        ServerClosed,
        /// Includes the ERROR text, e.g. "Closing Link: (Excess Flood)".
        ServerError(String),
        Dns(String),
        Refused,
        TimedOut,
        /// rustls rejected the chain; carries the leaf fingerprint for the
        /// phase-3 trust prompt.
        BadCertificate { reason: String, sha256: String },
        /// No traffic for 360 s after our idle PING at 270 s.
        PingTimeout,
        Keyring(String),
        Io(String),
    }

    /// Commands into the transport task.
    #[derive(Debug)]
    pub enum Command {
        /// Already validated with `to_wire` by the session. Flood-controlled.
        Send(Message),
        /// Write QUIT, then wait up to 5 s for EOF.
        Quit(Message),
    }

    // pub fn spawn(rt: &tokio::runtime::Handle, dial: Dial)
    //     -> (mpsc::UnboundedSender<Command>, mpsc::UnboundedReceiver<TransportEvent>)
    //
    // Owns: DNS, TCP, rustls (platform verifier), line framing (LineReader,
    // unchanged), PONG replies, idle PING/timeout, the flood-control token
    // bucket, and the QUIT grace period. Holds no IRC state.
}
use transport::{Dial, Epoch, LinkError, TransportEvent};

// ---------------------------------------------------------------------------
// src/model/mod.rs  -- the public surface the shell sees
// ---------------------------------------------------------------------------

/// Runtime id of a buffer (console, channel or query). Never reused within a
/// process, so a stale id in a GTK closure resolves to nothing, not to the
/// wrong buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BufferId(u64);

#[derive(Debug)]
pub enum Input {
    Transport { server: ServerId, epoch: Epoch, event: TransportEvent },
    Timer(TimerToken),
    /// Return in the input bar. `confirmed` is true on the second pass after
    /// the user accepted the flood prompt.
    Typed { buffer: BufferId, text: String, as_action: bool, confirmed: bool },
    Focus(Option<BufferId>),
    WindowActive(bool),
    Expanded { server: ServerId, expanded: bool },
    Connect(ServerId),
    Disconnect(ServerId),
    CloseBuffer(BufferId),
    AddServer(ServerConfig),
    EditServer(ServerConfig),
    RemoveServer(ServerId),
    /// gio::NetworkMonitor. `true` skips any pending backoff.
    NetworkAvailable(bool),
    /// Window closed: QUIT everything, then `Effect::Exit`.
    QuitApp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerToken {
    pub server: ServerId,
    pub epoch: Epoch,
    pub kind: TimerKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerKind {
    /// 240 s from TCP up to 001, then disconnect and try the next address.
    RegistrationWatchdog,
    Reconnect,
    /// 90 s wait for NickServ before autojoining anyway.
    IdentifyWait,
    QuitDeadline,
    SaveState,
}

/// What one `apply` produced. The shell projects `changes` first (in order),
/// then runs `effects` (in order).
#[derive(Debug, Default)]
pub struct Outcome {
    pub changes: Vec<Change>,
    pub effects: Vec<Effect>,
}

/// What the UI must redraw. Rows are re-read from `&World`, so most variants
/// carry only an id; only prints and member diffs carry payloads, because
/// those are incremental by nature.
#[derive(Debug)]
pub enum Change {
    BufferAdded { id: BufferId, parent: Option<BufferId>, index: usize },
    BufferRemoved(BufferId),
    /// Label, active/inactive, unread or highlight count changed.
    BufferChanged(BufferId),
    /// Render through the buffer's style and append. Unread for this line is
    /// already counted; the `BufferChanged` for it follows in the same
    /// Outcome, so the badge never shows a count for an unrendered line.
    Print { buffer: BufferId, line: Line },
    Topic { buffer: BufferId, topic: String },
    Members { buffer: BufferId, diff: MemberDiff },
    /// Core moved focus (new query from /query, closed the focused buffer).
    /// The shell sets its SelectionModel; a user click arrives as
    /// `Input::Focus` and is not echoed back.
    Focus(Option<BufferId>),
}

#[derive(Debug)]
pub enum Effect {
    Dial { server: ServerId, epoch: Epoch, dial: Dial },
    Send { server: ServerId, epoch: Epoch, message: Message },
    Quit { server: ServerId, epoch: Epoch, message: Message },
    /// Fire `Input::Timer(token)` at `at`. Never cancelled; stale epochs
    /// are ignored on arrival.
    Timer { at: Instant, token: TimerToken },
    SaveConfig,
    SaveState,
    AskFloodConfirm { buffer: BufferId, text: String, lines: usize, as_action: bool },
    Exit,
}

/// Positional diff against the sorted member order, applied 1:1 to a
/// `gio::ListStore` with `splice`. Sort policy lives in `members`, not GTK.
#[derive(Debug)]
pub enum MemberDiff {
    /// NAMES finished, or the channel was parted/reset: re-read the list.
    Reset,
    Insert { at: usize, row: MemberRow },
    Remove { at: usize },
    Update { at: usize, row: MemberRow },
    Move { from: usize, to: usize, row: MemberRow },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemberRow {
    pub nick: String,
    /// Highest PREFIX symbol, e.g. '@'.
    pub symbol: Option<char>,
    pub away: bool,
    pub ircop: bool,
}

/// Read model for one sidebar row. CSS classes derive from this in one
/// function (`ui::sidebar::row_classes`).
#[derive(Debug, Clone, PartialEq)]
pub struct BufferRow {
    pub id: BufferId,
    pub label: String,
    pub kind: BufferKindTag,
    pub active: bool,
    pub unread: u32,
    pub highlights: u32,
    pub link: Option<LinkTag>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferKindTag {
    Console,
    Channel,
    Query,
}

/// Console rows show the link phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkTag {
    Offline,
    Connecting,
    Registering,
    Online,
    Waiting,
    Quitting,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Title {
    pub title: String,
    pub subtitle: String,
}

/// Everything nick completion needs from the model, in Textual's order.
pub struct CompletionSource<'a> {
    pub members: Vec<(&'a str, f32)>,
    pub channels: Vec<&'a str>,
    pub network: Option<&'a str>,
    pub in_channel: bool,
}

pub struct World {
    config: Config,
    ui_state: UiState,
    servers: Vec<model::network::Network>,
    buffers: model::buffers::Buffers,
    focus: Option<BufferId>,
    window_active: bool,
    state_dirty: bool,
    quitting: bool,
}

impl World {
    /// Builds consoles and configured channel buffers, restores focus, and
    /// returns the auto-connect dials plus the first `SaveState` timer.
    pub fn new(config: Config, ui_state: UiState, now: Instant) -> (World, Outcome) {
        todo!()
    }

    /// The only writer of client state.
    pub fn apply(&mut self, input: Input, now: Instant) -> Outcome {
        // match input {
        //   Transport{server, epoch, ..} if stale(server, epoch) => Outcome::default(),
        //   Transport{Connected}      => network.on_connected -> registration::start
        //   Transport{Line(m)}        => network.on_line(m): registration while
        //                                Registering, session::handle once Online;
        //                                both return Vec<Routed> and Vec<Message>
        //   Transport{Closed(e)}      => network.on_closed(e): drop Session,
        //                                deactivate buffers, MemberDiff::Reset,
        //                                schedule Reconnect per reconnect::policy
        //   Typed{..}                 => input::plan(...) -> Sends | Flood | Local
        //   Focus(b)                  => zero b's counters, BufferChanged, title
        //   ...
        // }
        // Every Routed print goes through `self.print(...)`, the one funnel.
        todo!()
    }

    // ---- read model for the shell (no setters exist) ----
    pub fn servers(&self) -> impl Iterator<Item = BufferId> + '_ {
        std::iter::empty()
    }
    pub fn children(&self, console: BufferId) -> &[BufferId] {
        todo!()
    }
    pub fn row(&self, id: BufferId) -> Option<BufferRow> {
        todo!()
    }
    pub fn members(&self, channel: BufferId) -> Vec<MemberRow> {
        todo!()
    }
    pub fn topic(&self, channel: BufferId) -> Option<&str> {
        todo!()
    }
    pub fn title(&self) -> Title {
        todo!()
    }
    pub fn focus(&self) -> Option<BufferId> {
        self.focus
    }
    pub fn completion_source(&self, buffer: BufferId) -> CompletionSource<'_> {
        todo!()
    }
    pub fn config(&self) -> &Config {
        &self.config
    }
    pub fn ui_state(&self) -> &UiState {
        &self.ui_state
    }
}

pub mod model {
    use super::*;

    // -----------------------------------------------------------------------
    // src/model/buffers.rs
    // -----------------------------------------------------------------------
    pub mod buffers {
        use super::*;

        pub struct Buffers {
            next: u64,
            by_id: HashMap<BufferId, Buffer>,
        }

        pub struct Buffer {
            pub id: BufferId,
            pub server: ServerId,
            pub kind: BufferKind,
            pub unread: u32,
            pub highlights: u32,
        }

        pub enum BufferKind {
            /// The server row itself (Textual: the client tree item).
            Console { children: Vec<BufferId> },
            /// `config` is None for a channel joined but not saved (e.g. via
            /// INVITE-and-join from another client on a bouncer).
            Channel { name: Name, config: Option<ChannelId> },
            Query { nick: Name },
        }

        impl Buffers {
            /// Inserts channels before the first query (Textual's rule).
            pub fn add(&mut self, server: ServerId, console: BufferId, kind: BufferKind) -> (BufferId, usize) {
                todo!()
            }
            pub fn remove(&mut self, id: BufferId) -> Option<Buffer> {
                todo!()
            }
            /// Channel or query by key under one server.
            pub fn find(&self, console: BufferId, key: &Key) -> Option<BufferId> {
                todo!()
            }
            /// After a CASEMAPPING change re-keys names.
            pub fn rekey(&mut self, console: BufferId, map: Casemapping) {
                todo!()
            }
        }

        /// Counting rule from Textual's setUnreadState: nothing for the
        /// console, nothing when this buffer is focused in an active window.
        pub fn counts(buffer: &Buffer, focused: bool, window_active: bool, line: &Line) -> (u32, u32) {
            todo!()
        }
    }

    // -----------------------------------------------------------------------
    // src/model/network.rs  -- one per configured server, lives forever
    // -----------------------------------------------------------------------
    pub mod network {
        use super::*;
        use crate::model::registration::Registration;
        use crate::model::session::Session;

        pub struct Network {
            pub id: ServerId,
            pub console: BufferId,
            pub epoch: Epoch,
            pub link: Link,
            /// Round-robin cursor into `config.addresses`.
            pub next_address: usize,
            /// Failed attempts since the last 001; drives backoff.
            pub failures: u32,
            /// Channels joined when the link dropped, rejoined on reconnect
            /// alongside configured autojoins.
            pub rejoin: Vec<Name>,
        }

        /// The connection lifecycle. The `Session` exists only while a link
        /// is up, so "reset everything on disconnect" is a drop, not a list
        /// of fields to clear (Textual's resetAllPropertyValues).
        pub enum Link {
            Offline,
            Connecting,
            Registering { reg: Registration, session: Session },
            Online { session: Session },
            Waiting { until: Instant, last: Disconnected },
            Quitting { session: Option<Session>, then: AfterQuit },
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum AfterQuit {
            StayOffline,
            /// Config edit that needs a fresh connect, or /reconnect.
            Reconnect,
            RemoveServer,
            ExitApp,
        }

        #[derive(Debug, Clone, PartialEq)]
        pub enum Disconnected {
            Link(LinkError),
            RegistrationTimeout,
            /// Every nick, alternate and padding was rejected.
            NoUsableNick,
            /// 465 / ERROR with a ban reason.
            Banned(String),
        }

        /// Transitions. Each returns the effects to run; each is a no-op in
        /// a phase where it does not apply, so a doubled input converges.
        impl Network {
            pub fn connect(&mut self, cfg: &ServerConfig, now: Instant) -> Vec<Effect> {
                // Offline | Waiting -> Connecting, epoch += 1, Dial + watchdog
                todo!()
            }
            pub fn quit(&mut self, reason: Option<&str>, then: AfterQuit, now: Instant) -> Vec<Effect> {
                // Registering|Online -> Quitting(+QuitDeadline 3 s); Connecting|Waiting -> Offline
                todo!()
            }
            pub fn closed(&mut self, error: LinkError, cfg: &ServerConfig, now: Instant) -> (Disconnected, Vec<Effect>) {
                // drop session; reconnect::next_attempt decides Waiting vs Offline
                todo!()
            }
        }

        pub mod reconnect {
            use super::*;

            /// 5 s, 10 s, 20 s ... capped at 300 s, ±20 % jitter derived from
            /// (server id, failures), so tests are deterministic. None means
            /// stay offline: user quit, auto_reconnect off, bad certificate,
            /// banned, or "Excess Flood"/"SendQ" (Textual cancels these too).
            pub fn next_attempt(reason: &Disconnected, cfg: &ServerConfig, failures: u32, id: ServerId) -> Option<Duration> {
                todo!()
            }
        }
    }

    // -----------------------------------------------------------------------
    // src/model/registration.rs  -- CAP LS 302, SASL, nick fallback (pure)
    // -----------------------------------------------------------------------
    pub mod registration {
        use super::*;

        pub const WANTED: &[&str] = &[
            "away-notify",
            "batch",
            "cap-notify",
            "chghost",
            "echo-message",
            "extended-join",
            "multi-prefix",
            "sasl",
            "server-time",
            "userhost-in-names",
            "znc.in/self-message",
        ];

        pub struct Registration {
            pub caps: CapPhase,
            pub sasl: SaslPhase,
            pub nick: NickAttempt,
            /// Dropped (with the secrets) when registration ends.
            pub secrets: Secrets,
        }

        pub enum CapPhase {
            /// Accumulating `CAP * LS * :...` continuation lines. Nothing is
            /// REQ'd until the final line (Textual REQs after each chunk).
            Listing { offered: Vec<(String, Option<String>)> },
            /// One or more batched REQ lines out; each is ACKed or NAKed whole.
            Requesting { outstanding: Vec<Vec<String>> },
            /// AUTHENTICATE in progress; CAP END waits for it.
            Authenticating,
            Ended,
        }

        pub enum SaslPhase {
            NotUsed,
            Offered { mechanisms: Vec<String> },
            Sent { mechanism: Mechanism, tried: Vec<Mechanism> },
            Succeeded { account: String },
            /// 902/904/905/906 or 908 with nothing left to try. Registration
            /// continues (Textual's behaviour) and the console says why;
            /// `autojoin_waits_for_identify` then holds autojoin until the
            /// IdentifyWait timer.
            Failed { numeric: u16, text: String },
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Mechanism {
            Plain,
            External,
        }

        pub enum NickAttempt {
            Primary,
            Alternate(usize),
            /// Primary padded with `_` up to NICKLEN.
            Padded(usize),
        }

        /// What one inbound line did during registration.
        pub enum Step {
            Continue(Vec<Message>),
            /// 001 arrived: become Online with this nick.
            Registered { nick: String, out: Vec<Message> },
            Fail(network::Disconnected),
        }

        /// `CAP LS 302`, `PASS`, `NICK`, `USER`.
        pub fn start(cfg: &ServerConfig, secrets: Secrets) -> (Registration, Vec<Message>) {
            todo!()
        }
        /// CAP, AUTHENTICATE, 90x, 433/432/437, 001, 465, ERROR. Other lines
        /// (NOTICE AUTH, 020) pass to the session for printing.
        pub fn step(reg: &mut Registration, cfg: &ServerConfig, m: &Message) -> Step {
            todo!()
        }
        /// Greedy packing of cap names into `CAP REQ :a b c` lines ≤ 510 bytes.
        pub fn pack_req(caps: &[&str]) -> Vec<Message> {
            todo!()
        }
        /// base64(`account\0account\0password`) in 400-byte chunks, with a
        /// trailing `+` when the last chunk is exactly 400 bytes.
        pub fn sasl_plain_payload(account: &str, password: &str) -> Vec<Message> {
            todo!()
        }
    }

    // -----------------------------------------------------------------------
    // src/model/session.rs  -- per-connection protocol state
    // -----------------------------------------------------------------------
    pub mod session {
        use super::*;
        use crate::model::members::Members;

        pub struct Session {
            pub nick: Name,
            /// From our own JOIN echo, 396 or WHO; None until seen. Sizes the
            /// split budget.
            pub hostmask: Option<String>,
            pub caps: HashSet<String>,
            pub isupport: Isupport,
            pub channels: HashMap<Key, ChannelState>,
            /// Network-wide user table: one record per nick seen in a
            /// channel we're in or an open query. Removed when neither holds.
            pub users: HashMap<Key, User>,
            pub away: bool,
            /// WHO requests we sent ourselves, whose replies are not printed.
            pub hidden_who: HashSet<Key>,
            pub identified: bool,
            pub autojoined: bool,
        }

        pub struct ChannelState {
            pub buffer: BufferId,
            pub name: Name,
            pub topic: Option<String>,
            pub modes: Vec<(char, Option<String>)>,
            pub members: Members,
            /// 353 lines collect here; 366 swaps them in whole. A later
            /// /names refreshes too (Textual ignores NAMES after the first).
            pub names: Option<Vec<(Name, u16 /* mode bits */, Option<(String, String)>)>>,
        }

        pub struct User {
            pub nick: Name,
            pub user: Option<String>,
            pub host: Option<String>,
            pub realname: Option<String>,
            pub account: Option<String>,
            pub away: bool,
            pub ircop: bool,
            pub channels: HashSet<Key>,
            /// Nick-completion weight, halved per minute (Textual's
            /// conversation weights, lazily decayed on read).
            pub weight: (f32, Instant),
        }

        /// Where a printable line goes, resolved against state *before* the
        /// state update (QUIT/NICK need the channels the nick was in).
        #[derive(Debug)]
        pub enum Route {
            Buffer(BufferId),
            Console,
            /// Incoming PM: open (or reuse) the query, then print.
            Query(Name),
            /// Message to a channel we're not in: dropped (Textual).
            Drop,
        }

        pub struct Handled {
            pub prints: Vec<(Route, Line)>,
            pub send: Vec<Message>,
            pub changes: Vec<Change>,
            /// Self JOIN of an unknown channel, self NICK (title), etc.
            pub events: Vec<SessionEvent>,
        }

        pub enum SessionEvent {
            Joined(Name),
            Parted(Key),
            NickChanged,
            Identified,
            ConfigChannelSeen(Name),
        }

        impl Session {
            pub fn new(nick: &str, caps: HashSet<String>) -> Session {
                todo!()
            }
            /// The receive switch. Route first, then mutate. Echo-message:
            /// PRIVMSG from our own nick is printed here (from_self), and
            /// `World` skips the local echo when the cap is on.
            pub fn handle(&mut self, m: &Message, buffers: &mut buffers::Buffers, now: Instant) -> Handled {
                todo!()
            }
            /// 005 with a new CASEMAPPING re-keys channels, users, members.
            fn rekey(&mut self, map: Casemapping) {
                todo!()
            }
            pub fn key(&self, name: &str) -> Key {
                self.isupport.casemapping.fold(name)
            }
        }
    }

    // -----------------------------------------------------------------------
    // src/model/members.rs  -- sorted membership with positional diffs
    // -----------------------------------------------------------------------
    pub mod members {
        use super::*;

        /// Modes as a bitset over `Isupport::prefix` index (bit 0 = highest
        /// rank), so "highest mode" is `trailing_zeros` and there is no
        /// rank-ordered string to keep sorted (Textual's `modes[0]` invariant).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        pub struct ModeBits(pub u16);

        impl ModeBits {
            pub fn highest(self) -> Option<u8> {
                (self.0 != 0).then(|| self.0.trailing_zeros() as u8)
            }
        }

        /// Sort key: rank (lower index first, none last), then folded nick.
        /// Optional ircop-first lands with preferences in phase 3.
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
        pub struct SortKey(u8, Key);

        pub struct Members {
            modes: HashMap<Key, ModeBits>,
            /// Always sorted; positions are what `MemberDiff` refers to.
            order: Vec<SortKey>,
        }

        impl Members {
            pub fn insert(&mut self, key: Key, modes: ModeBits, row: MemberRow) -> Option<MemberDiff> {
                todo!()
            }
            pub fn remove(&mut self, key: &Key) -> Option<MemberDiff> {
                todo!()
            }
            pub fn set_modes(&mut self, key: &Key, modes: ModeBits, row: MemberRow) -> Option<MemberDiff> {
                todo!()
            }
            /// NICK: one Move (or Update when the position is unchanged).
            pub fn rename(&mut self, old: &Key, new: Key, row: MemberRow) -> Option<MemberDiff> {
                todo!()
            }
            pub fn replace_all(&mut self, entries: Vec<(Key, ModeBits)>) -> MemberDiff {
                todo!()
            }
            pub fn len(&self) -> usize {
                self.order.len()
            }
            pub fn iter(&self) -> impl Iterator<Item = &Key> {
                self.order.iter().map(|k| &k.1)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// src/input/  -- pure, no GTK: commands, splitting, history, completion
// ---------------------------------------------------------------------------
pub mod input {
    use super::*;

    // ---- commands.rs: one table for dispatch AND completion ----
    pub struct CommandSpec {
        pub name: &'static str,
        pub aliases: &'static [&'static str],
        pub run: fn(&mut CommandCtx<'_>, &str) -> Result<(), CommandError>,
    }

    pub struct CommandCtx<'a> {
        pub buffer: BufferId,
        pub out: &'a mut Vec<Planned>,
    }

    #[derive(Debug)]
    pub enum CommandError {
        Usage(&'static str),
        NotConnected,
        NeedsChannel,
    }

    /// join, part, msg, query, me, notice, nick, topic, quit, close,
    /// connect, disconnect, reconnect, away, raw/quote, clear. Anything
    /// else is sent as `UPPERCMD rest`.
    pub static COMMANDS: &[CommandSpec] = &[];

    /// Pure planning of a typed line; `World` executes the plan.
    #[derive(Debug, PartialEq)]
    pub enum Planned {
        Text { target: String, text: String, action: bool, notice: bool },
        Raw(Message),
        Join { channels: Vec<String>, key: Option<String> },
        Part { channel: Option<String>, reason: Option<String> },
        OpenQuery { nick: String, text: Option<String> },
        Close,
        Connect,
        Disconnect { reason: Option<String> },
        Reconnect,
        Clear,
        LocalError(String),
    }

    pub enum Plan {
        Run(Vec<Planned>),
        /// More than 4 lines or 4×510 bytes: ask first (Textual's flood alert).
        Confirm { lines: usize },
    }

    /// Console: every line is a command, `/` optional. Channel/query:
    /// `/cmd` runs, `//x` sends "/x", a lone "/" is text.
    pub fn plan(text: &str, in_console: bool, confirmed: bool, as_action: bool) -> Plan {
        todo!()
    }

    // ---- split.rs ----
    /// Bytes left for the trailing text of `PRIVMSG <target> :` given our
    /// real hostmask, or a 60-byte guess before we know it (Textual).
    pub fn text_budget(hostmask: Option<&str>, nick: &str, command: &str, target: &str, action: bool) -> usize {
        todo!()
    }
    /// Splits on char boundaries, never inside a `\x03NN,NN` colour code,
    /// preferring the last space within 25 bytes of the limit. Each chunk
    /// passes `Message::to_wire` (CR/LF already split into lines).
    pub fn split(text: &str, budget: usize) -> Vec<String> {
        todo!()
    }

    // ---- history.rs ----
    /// Textual's TLOInputHistory, without its two bugs: consecutive
    /// duplicates are dropped, and a draft is restored, not pushed as an
    /// entry. Cap 100. One per buffer (Textual's channel-specific mode,
    /// which also gives per-buffer drafts); a global mode is a phase-3 pref.
    #[derive(Default)]
    pub struct History {
        entries: VecDeque<String>,
        position: usize,
        draft: Option<String>,
    }
    impl History {
        pub fn push(&mut self, sent: &str) {
            todo!()
        }
        /// `current` is the field text; returns the text to show, or None.
        pub fn up(&mut self, current: &str) -> Option<String> {
            todo!()
        }
        pub fn down(&mut self, current: &str) -> Option<String> {
            todo!()
        }
    }

    // ---- completion.rs ----
    /// Textual's TLONicknameCompletionStatus as a value. The shell keeps one
    /// per input bar and passes the field each Tab; a mismatch between the
    /// field and `left` resets the cycle, exactly as Textual's cache check.
    #[derive(Default)]
    pub struct Completion {
        left: Option<(String, usize)>,
        candidates: Vec<String>,
        index: usize,
        span: (usize, usize),
    }

    pub struct Field<'a> {
        pub text: &'a str,
        pub caret: usize,
    }

    #[derive(Debug, PartialEq)]
    pub struct Edit {
        pub replace: (usize, usize),
        pub with: String,
        pub caret: usize,
    }

    #[derive(Clone, Copy)]
    pub enum Dir {
        Forward,
        Back,
    }

    impl Completion {
        /// `/cmd` at line start from `COMMANDS`; `#chan` from channels;
        /// nicks (with `@`) in channels only, weight-ordered, plus trimmed
        /// forms (`_foo`→`foo`) and the service names; suffix ": " only for
        /// a nick at line start.
        pub fn tab(&mut self, field: Field<'_>, source: &CompletionSource<'_>, suffix: &str, dir: Dir) -> Option<Edit> {
            todo!()
        }
        pub fn reset(&mut self) {
            *self = Completion::default();
        }
    }
}

// ---------------------------------------------------------------------------
// The shell's dispatch loop (src/ui/app.rs), sketched without GTK types.
// ---------------------------------------------------------------------------
pub mod shell_contract {
    use super::*;

    /// What `ui::App` implements. Split out only so this sketch type-checks.
    pub trait Projection {
        fn project(&mut self, world: &World, change: &Change);
        fn execute(&mut self, effect: Effect, dispatch_later: &mut VecDeque<Input>);
    }

    /// Re-entrancy-safe dispatch: a GTK signal fired while projecting (for
    /// example selection-changed after `Change::Focus`) queues its input
    /// instead of borrowing `World` again.
    pub fn dispatch(world: &mut World, ui: &mut impl Projection, first: Input) {
        let mut queue = VecDeque::from([first]);
        while let Some(input) = queue.pop_front() {
            let outcome = world.apply(input, Instant::now());
            for change in &outcome.changes {
                ui.project(world, change);
            }
            for effect in outcome.effects {
                ui.execute(effect, &mut queue);
            }
        }
    }
}

fn main() {}
