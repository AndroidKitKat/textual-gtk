//! textual-gtk phase 2, candidate 2: "one reducer per network on the main
//! thread, a dumb transport on tokio, effects into GTK".
//!
//! Types and signatures only. Bodies are `todo!()`. `mod existing` stands in
//! for `src/irc/message.rs`, `src/line.rs` and `src/style`, which stay as they
//! are apart from the two signature changes called out in design.md.
//!
//! # Usage, written before the types
//!
//! The socket line to pixel path, as `ui/world.rs` runs it:
//!
//! ```ignore
//! // A transport event arrived on the glib main loop for network `net`.
//! fn on_transport(world: &World, net: NetworkId, conn: ConnectionId, event: transport::Event) {
//!     world.dispatch(net, Input::Transport { conn, event });
//! }
//!
//! impl World {
//!     pub fn dispatch(&self, net: NetworkId, input: Input) {
//!         // Borrow, reduce, release. Effects are applied with no borrow held,
//!         // so a GTK signal fired by an effect may dispatch again.
//!         let effects = self.inner.borrow_mut().slots[&net].session.handle(input, self.clock.now());
//!         for effect in effects {
//!             self.apply(net, effect);
//!         }
//!     }
//!
//!     fn apply(&self, net: NetworkId, effect: Effect) {
//!         match effect {
//!             Effect::Send(message) => self.transport(net).send(message),
//!             Effect::Connect { conn, endpoint } => self.open_transport(net, conn, endpoint),
//!             Effect::Close => self.transport(net).close(),
//!             Effect::Print { buffer, line } => {
//!                 self.view(net, buffer).append(line.render(&self.style), &line.id);
//!                 self.sidebar.count(net, buffer, &line);          // unread + highlight, unless focused and active
//!             }
//!             Effect::Buffer(id, change) => self.sidebar.apply_buffer(net, id, change),
//!             Effect::Members(id, change) => self.member_list.apply(net, id, change),
//!             Effect::Status(status) => { self.sidebar.set_status(net, &status); self.window.refresh_title(); }
//!             Effect::Persist(change) => self.state_store.record(net, change), // throttled save
//!         }
//!     }
//! }
//! ```
//!
//! The input field, Return pressed in buffer `focus` (Enter in `ui/input.rs`):
//!
//! ```ignore
//! let text = input_bar.take_text();          // multi-line, formatting codes kept
//! history.add(focus, &text);
//! world.dispatch(focus.network, Input::Typed { buffer: focus.buffer, text });
//! // Session::handle parses slash commands, splits to the byte budget, prints
//! // locally unless echo-message is on, and returns Effect::Send per line.
//! ```
//!
//! Tab in the input field:
//!
//! ```ignore
//! let field = input_bar.snapshot();          // text, caret, selection
//! let session = world.session(focus.network); // read-only borrow
//! let edit = completion.step(&field, Direction::Forward, &session.completion_source(focus.buffer));
//! if let Some(edit) = edit { input_bar.apply(edit); }
//! ```
//!
//! A headless test, no GTK, no socket, no timers (`tests/session.rs`):
//!
//! ```ignore
//! let mut s = Session::new(fixtures::network("libera"), Secrets::none());
//! let mut t = Script::new(&mut s);           // feeds `S:` lines, asserts `C:` sends, advances the clock
//! t.user(UserAction::Connect);
//! t.expect_connect();                        // Effect::Connect
//! t.transport(transport::Event::Connected { secured: true });
//! t.expect_sent(&["CAP LS 302", "NICK tester", "USER tuser 0 * :Test User"]);
//! t.server(":irc CAP * LS * :sasl=PLAIN,EXTERNAL multi-prefix");
//! t.server(":irc CAP * LS :server-time echo-message");
//! t.expect_sent(&["CAP REQ :sasl multi-prefix server-time echo-message"]); // one REQ, after the last LS chunk
//! ```

#![allow(dead_code, unused_variables, clippy::all)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

// ---------------------------------------------------------------------------
// Stand-ins for code that already exists in src/.
// ---------------------------------------------------------------------------
pub mod existing {
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Message {
        pub tags: Vec<(String, String)>,
        pub source: Option<String>,
        pub command: String,
        pub params: Vec<String>,
    }
    impl Message {
        pub fn new(command: &str, params: &[&str]) -> Self {
            todo!()
        }
        pub fn param(&self, i: usize) -> Option<&str> {
            todo!()
        }
        pub fn tag(&self, key: &str) -> Option<&str> {
            todo!()
        }
    }
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct WireError;

    #[derive(Debug, Clone, PartialEq)]
    pub struct Line {
        pub id: String,
        pub body: String,
        pub from_self: bool,
        pub highlight: bool,
    }
    pub struct Style;
    impl Line {
        pub fn render(&self, style: &Style) -> String {
            todo!()
        }
    }
    /// Unchanged shape. `from_message` gains a `&CaseMapping` so "mine" and
    /// channel targets fold correctly (design.md, "Changes to existing code").
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Destination {
        Channel(String),
        ChannelsOf(String),
        Server,
    }
}
use existing::{Line, Message};

// ---------------------------------------------------------------------------
// id: persisted identities. uuid v4 in the real crate.
// ---------------------------------------------------------------------------
pub mod id {
    /// A random 128-bit id, written as a UUID string in config and state files
    /// and as the `id` attribute of libsecret items.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
    pub struct Uuid([u8; 16]);

    /// One configured network ("client" in Textual). Also the id of its
    /// console buffer, so a console needs no second stored id.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct NetworkId(pub Uuid);

    /// One buffer (console, channel or query) inside a network. Channel ids
    /// come from config and survive restarts; query ids are minted when the
    /// query opens. Only meaningful together with its NetworkId.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct BufferId(pub Uuid);

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ServerAddressId(pub Uuid);

    /// A buffer address the UI can hold without a borrow of any session.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct BufferRef {
        pub network: NetworkId,
        pub buffer: BufferId,
    }

    /// Counts transport generations within one Session. A late event from a
    /// socket the session already gave up on carries an old id and is dropped.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ConnectionId(pub u32);
}
use id::*;

// ---------------------------------------------------------------------------
// irc::casemap: identity under CASEMAPPING.
// ---------------------------------------------------------------------------
pub mod casemap {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum CaseMapping {
        Ascii,
        /// `[]\~` fold to `{}|^`. The default until 005 says otherwise.
        #[default]
        Rfc1459,
        /// `[]\` fold, `~` does not.
        Rfc1459Strict,
    }

    /// A nick folded under one CaseMapping. Built only by `CaseMapping::nick`,
    /// so a map keyed by it cannot be probed with a raw string.
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub struct NickKey(Box<str>);

    /// A channel name folded the same way. A distinct type from NickKey so a
    /// nick is never looked up in the channel table.
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub struct ChanKey(Box<str>);

    impl CaseMapping {
        pub fn parse(isupport_value: &str) -> Option<Self> {
            todo!()
        }
        pub fn nick(self, raw: &str) -> NickKey {
            todo!()
        }
        pub fn chan(self, raw: &str) -> ChanKey {
            todo!()
        }
        pub fn eq(self, a: &str, b: &str) -> bool {
            todo!()
        }
    }
}
use casemap::{CaseMapping, ChanKey, NickKey};

// ---------------------------------------------------------------------------
// irc::isupport: 005 tokens, merged additively, reset per connection.
// ---------------------------------------------------------------------------
pub mod isupport {
    use super::casemap::CaseMapping;

    /// One PREFIX entry. Rank is highest first, so `(ov)@+` gives o=1, v=0.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Prefix {
        pub mode: char,
        pub symbol: char,
        pub rank: u8,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ISupport {
        pub casemapping: CaseMapping,
        pub network: Option<String>,
        /// Ordered highest rank first. Default `(ov)@+`.
        pub prefixes: Vec<Prefix>,
        /// CHANMODES groups A..D (list, always-param, param-when-set, flag).
        pub chanmodes: [Vec<char>; 4],
        /// CHANTYPES. Default `#&`.
        pub chantypes: Vec<char>,
        pub statusmsg: Vec<char>,
        pub nicklen: Option<usize>,
        pub topiclen: Option<usize>,
        pub kicklen: Option<usize>,
        pub awaylen: Option<usize>,
        pub channellen: Option<usize>,
        /// MODES: how many parameterised modes per MODE line. Default 3.
        pub modes_per_line: usize,
        pub monitor: Option<usize>,
        pub raw: Vec<(String, Option<String>)>,
    }

    impl Default for ISupport {
        fn default() -> Self {
            todo!()
        }
    }

    impl ISupport {
        /// Applies the tokens of one 005 line (params minus nick and trailing
        /// text). `-TOKEN` removes. Returns whether CASEMAPPING changed, which
        /// makes the session re-key its tables.
        pub fn apply(&mut self, tokens: &[&str]) -> bool {
            todo!()
        }
        pub fn is_channel(&self, name: &str) -> bool {
            todo!()
        }
        /// Strips leading STATUSMSG/PREFIX symbols off a message target.
        pub fn strip_statusmsg<'a>(&self, target: &'a str) -> &'a str {
            todo!()
        }
        pub fn prefix_for_symbol(&self, symbol: char) -> Option<Prefix> {
            todo!()
        }
        pub fn prefix_for_mode(&self, mode: char) -> Option<Prefix> {
            todo!()
        }
        /// Whether a channel mode takes a parameter when set or unset.
        pub fn mode_takes_param(&self, mode: char, set: bool) -> bool {
            todo!()
        }
    }
}
use isupport::ISupport;

// ---------------------------------------------------------------------------
// irc::modes: mode strings parsed against ISUPPORT.
// ---------------------------------------------------------------------------
pub mod modes {
    use super::isupport::ISupport;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ModeChange {
        pub set: bool,
        pub mode: char,
        pub param: Option<String>,
    }

    /// `+ov-b alice bob *!*@spam` into one change per letter. Unknown modes
    /// are kept without a parameter so printing still works.
    pub fn parse(isupport: &ISupport, modes: &str, params: &[&str]) -> Vec<ModeChange> {
        todo!()
    }

    /// Channel modes without members: `+nt`, `+k key`, `+l 50`. Lists (bans)
    /// are not tracked in phase 2.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct ChannelModes {
        pub flags: Vec<(char, Option<String>)>,
    }

    impl ChannelModes {
        pub fn apply(&mut self, change: &ModeChange) {
            todo!()
        }
        /// `+ntk key`, with the key masked when `mask_key` (title bar).
        pub fn display(&self, mask_key: bool) -> String {
            todo!()
        }
    }
}
use modes::{ChannelModes, ModeChange};

// ---------------------------------------------------------------------------
// irc::caps: CAP LS 302 accumulation and one batched REQ. Pure.
// ---------------------------------------------------------------------------
pub mod caps {
    use std::collections::{HashMap, HashSet};

    /// Capabilities this client knows how to use. Textual's list minus the
    /// ZNC extras, plus cap-notify which 302 implies.
    pub const WANTED: &[&str] = &[
        "sasl",
        "multi-prefix",
        "userhost-in-names",
        "away-notify",
        "chghost",
        "server-time",
        "batch",
        "echo-message",
        "cap-notify",
        "account-notify",
    ];

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum CapPhase {
        /// Collecting `CAP * LS *` chunks. Nothing is requested until the
        /// final chunk (no `*`), which fixes Textual's early REQ/END.
        Listing,
        /// One REQ out, waiting for ACK or NAK.
        Requested(Vec<String>),
        /// Waiting on SASL before CAP END.
        Authenticating,
        /// CAP END sent (or no caps offered).
        Ended,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Caps {
        pub phase: CapPhase,
        /// Everything the server offered, with values (`sasl=PLAIN,EXTERNAL`).
        pub offered: HashMap<String, Option<String>>,
        pub enabled: HashSet<String>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum CapStep {
        /// Send `CAP REQ :<space separated>`; split at 510 bytes by the
        /// caller if ever needed.
        Request(Vec<String>),
        /// Start SASL with this mechanism before ending.
        StartSasl(String),
        End,
        Nothing,
    }

    impl Caps {
        pub fn new() -> Self {
            todo!()
        }
        /// Feed one `CAP` line's subcommand and args. `want_sasl` says whether
        /// credentials exist for a mechanism in `offered["sasl"]`.
        pub fn on_cap(&mut self, subcommand: &str, args: &[&str], want_sasl: Option<&str>) -> CapStep {
            todo!()
        }
        /// After SASL finished, success or not: the policy is "continue".
        pub fn sasl_done(&mut self) -> CapStep {
            todo!()
        }
        pub fn has(&self, cap: &str) -> bool {
            todo!()
        }
    }
}
use caps::{CapStep, Caps};

// ---------------------------------------------------------------------------
// irc::sasl: PLAIN and EXTERNAL as a step function. Pure.
// ---------------------------------------------------------------------------
pub mod sasl {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Mechanism {
        /// authzid = authcid = username (ident), password = nickserv password,
        /// matching Textual.
        Plain { username: String, password: super::config::Secret },
        External,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SaslState {
        Started(Mechanism),
        /// `AUTHENTICATE +` received, payload chunks of 400 bytes queued.
        Sending(std::collections::VecDeque<String>),
        Succeeded { account: Option<String> },
        Failed { numeric: u16, text: String },
    }

    impl SaslState {
        /// Returns the `AUTHENTICATE` lines to send for this server line, and
        /// whether the exchange is over.
        pub fn step(&mut self, command: &str, params: &[&str]) -> (Vec<String>, bool) {
            todo!()
        }
    }
}
use sasl::SaslState;

// ---------------------------------------------------------------------------
// irc::transport: tokio task, bytes to lines and back. No protocol knowledge.
// ---------------------------------------------------------------------------
pub mod transport {
    use super::existing::{Message, WireError};
    use std::time::Duration;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Tls {
        Off,
        /// rustls + system roots (rustls-native-certs), SNI = host.
        Verified,
        /// validateServerCertificateChain = false.
        Unverified,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Endpoint {
        pub host: String,
        pub port: u16,
        pub tls: Tls,
        pub connect_timeout: Duration,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub enum Event {
        /// TCP (and TLS, when on) is up. Nothing has been sent.
        Connected { secured: bool },
        Line(Message),
        /// A `Send` that could not be framed.
        Refused { message: Message, reason: WireError },
        /// Always last.
        Closed(Closed),
    }

    #[derive(Debug, Clone, PartialEq)]
    pub enum Closed {
        /// We asked (`Command::Close`), or dropped the handle.
        ByUs,
        Eof,
        /// Includes connect timeouts and refused connections.
        Io(String),
        /// TLS handshake rejected the chain. Details for a later trust prompt.
        BadCertificate(String),
    }

    #[derive(Debug, Clone)]
    pub enum Command {
        Send(Message),
        /// Flush what is queued, then shut the socket. The task stays alive for
        /// `grace` so a server reply (ERROR after QUIT) still arrives.
        Close { grace: Duration },
    }

    /// The task's ends. Events are drained on the glib main loop.
    pub struct Handle {
        pub commands: tokio::sync::mpsc::UnboundedSender<Command>,
        pub events: tokio::sync::mpsc::UnboundedReceiver<Event>,
    }

    /// Connects, frames lines, writes lines. Registration, PING, retries and
    /// autojoin moved to `session`. Flood control is a phase-3 insertion
    /// point here, behind `Command::Send`.
    pub fn spawn(runtime: &tokio::runtime::Handle, endpoint: Endpoint) -> Handle {
        todo!()
    }
}
use transport::Endpoint;

// ---------------------------------------------------------------------------
// config: what the user configures, on disk as TOML. Secrets live elsewhere.
// ---------------------------------------------------------------------------
pub mod config {
    use super::id::*;
    use std::path::PathBuf;

    /// A password in memory. Debug prints `[redacted]`; it never derives
    /// Serialize, so it cannot land in config.toml by accident.
    #[derive(Clone, PartialEq, Eq)]
    pub struct Secret(String);
    impl std::fmt::Debug for Secret {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("[redacted]")
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ServerAddress {
        pub id: ServerAddressId,
        pub host: String,
        pub port: u16,
        pub tls: bool,
        /// Loaded from the keyring at startup (`kind=server-password`).
        pub password: Option<Secret>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ChannelKind {
        Channel,
        Query,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ChannelConfig {
        pub id: BufferId,
        pub name: String,
        pub kind: ChannelKind,
        pub autojoin: bool,
        /// Keyring `kind=channel-key`.
        pub key: Option<Secret>,
        pub default_modes: Option<String>,
        pub default_topic: Option<String>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Identity {
        pub nick: String,
        pub alternate_nicks: Vec<String>,
        pub username: String,
        pub realname: String,
        /// NickServ password and SASL PLAIN password. Keyring `kind=nickserv`.
        pub nick_password: Option<Secret>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct NetworkConfig {
        pub id: NetworkId,
        pub name: String,
        pub servers: Vec<ServerAddress>,
        pub identity: Identity,
        pub channels: Vec<ChannelConfig>,
        pub auto_connect: bool,
        pub auto_reconnect: bool,
        pub autojoin_waits_for_nickserv: bool,
        pub on_connect_commands: Vec<String>,
        pub validate_certificate_chain: bool,
        pub echo_message: bool,
        pub sidebar_expanded: bool,
    }

    /// `$XDG_CONFIG_HOME/textual-gtk/config.toml`. Written only on explicit
    /// edits and at quit. Defaults are stripped on save (serde skip_if).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Config {
        pub version: u32,
        pub networks: Vec<NetworkConfig>,
        pub style: String,
        pub input_history_per_buffer: bool,
        pub completion_suffix: String,
    }

    /// `$XDG_STATE_HOME/textual-gtk/state.toml`. Runtime facts that change
    /// often and are safe to lose: a separate file so their throttled writes
    /// never rewrite config.toml.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct RuntimeState {
        pub selected: Option<BufferRef>,
        pub window: Option<(i32, i32, bool)>,
        pub sidebar_width: Option<i32>,
        pub member_list_visible: bool,
        /// Per network, for ZNC playback (`lastMessageServerTime`).
        pub last_message_time: Vec<(NetworkId, String)>,
    }

    #[derive(Debug)]
    pub enum ConfigError {
        Io(std::io::Error),
        Parse(String),
        /// The file's version is newer than this build understands.
        TooNew(u32),
    }

    pub struct Paths {
        pub config: PathBuf,
        pub state: PathBuf,
    }

    impl Paths {
        /// XDG dirs, overridable by `TEXTUAL_GTK_CONFIG_DIR` for tests.
        pub fn discover() -> Self {
            todo!()
        }
    }

    impl Config {
        /// Parses, runs `migrate` from the file's version up to CURRENT, then
        /// fills secrets from `secrets`. A missing file is `Config::default()`.
        pub fn load(paths: &Paths, secrets: &dyn Secrets) -> Result<Self, ConfigError> {
            todo!()
        }
        /// Atomic (tmp + rename). Secrets are stripped and written through
        /// `secrets` keyed by owner id.
        pub fn save(&self, paths: &Paths, secrets: &dyn Secrets) -> Result<(), ConfigError> {
            todo!()
        }
        pub fn default() -> Self {
            todo!()
        }
    }

    /// One step per version. Applied in order at load; `toml::Table` in, out.
    pub fn migrate(version: u32, table: &mut toml_stub::Table) -> Result<u32, ConfigError> {
        todo!()
    }
    pub mod toml_stub {
        pub struct Table;
    }

    impl RuntimeState {
        pub fn load(paths: &Paths) -> Self {
            todo!()
        }
        pub fn save(&self, paths: &Paths) -> Result<(), ConfigError> {
            todo!()
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SecretKind {
        ServerPassword,
        NickServ,
        ChannelKey,
    }

    /// libsecret through `oo7` in the app; an in-memory map in tests and e2e
    /// (`TEXTUAL_GTK_SECRETS=memory`). Items carry attributes
    /// `{application: textual-gtk, kind, id}`.
    pub trait Secrets {
        fn get(&self, kind: SecretKind, owner: Uuid) -> Option<Secret>;
        fn set(&self, kind: SecretKind, owner: Uuid, value: Option<&Secret>);
    }
    pub struct Keyring;
    pub struct MemorySecrets;
    impl Secrets for Keyring {
        fn get(&self, kind: SecretKind, owner: Uuid) -> Option<Secret> {
            todo!()
        }
        fn set(&self, kind: SecretKind, owner: Uuid, value: Option<&Secret>) {
            todo!()
        }
    }
    impl Secrets for MemorySecrets {
        fn get(&self, kind: SecretKind, owner: Uuid) -> Option<Secret> {
            todo!()
        }
        fn set(&self, kind: SecretKind, owner: Uuid, value: Option<&Secret>) {
            todo!()
        }
    }
}
use config::{ChannelConfig, NetworkConfig, Secret};

// ---------------------------------------------------------------------------
// session: the reducer. One per network, owned by World on the main thread.
// ---------------------------------------------------------------------------
pub mod session {
    use super::*;

    // ---- state -----------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ChannelStatus {
        /// Configured, not joined. Buffer exists, member list empty.
        Parted,
        /// JOIN sent, no self JOIN yet.
        Joining,
        Joined,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Topic {
        pub text: String,
        pub set_by: Option<String>,
        pub set_at: Option<SystemTime>,
    }

    /// One membership. Display nick keeps the server's case.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Member {
        pub nick: String,
        pub user: Option<String>,
        pub host: Option<String>,
        pub realname: Option<String>,
        pub account: Option<String>,
        /// Mode letters, highest rank first, so `modes.first()` is the prefix
        /// to draw. Kept sorted by `set_mode`.
        pub modes: Vec<char>,
        pub away: bool,
        pub ircop: bool,
        /// Textual's conversation weight for completion order (+20/+5 when
        /// mentioned, +4/+1 otherwise); halves per minute from `weight_at`.

        pub weight: u32,
        pub weight_at: Option<Instant>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Channel {
        pub id: BufferId,
        pub name: String,
        pub status: ChannelStatus,
        pub topic: Option<Topic>,
        pub modes: ChannelModes,
        /// Unordered. Sorting is the reader's job (UI sorter, completion).
        pub members: HashMap<NickKey, Member>,
        /// 366 seen for the current join; a later NAMES replaces, not appends.
        pub names_complete: bool,
        pub names_pending: HashMap<NickKey, Member>,
        pub join_error: Option<String>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Query {
        pub id: BufferId,
        pub nick: String,
        pub hostmask: Option<String>,
        pub online: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BufferKind {
        Console,
        Channel,
        Query,
    }

    /// Why the last connection ended, kept for the status line and the
    /// reconnect decision.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Ended {
        UserQuit,
        ServerClosed,
        Io(String),
        BadCertificate(String),
        /// ERROR :Excess Flood / SendQ: no auto reconnect, like Textual.
        Killed(String),
        RegistrationTimeout,
        PingTimeout,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Registration {
        pub caps: Caps,
        pub sasl: Option<SaslState>,
        /// Index into [nick, alternates...], then `_` padding, then "0".
        pub nick_attempt: usize,
        pub current_nick: String,
        pub started: Instant,
    }

    /// The connection state machine. Exactly one variant at a time; every
    /// timer deadline lives in the variant that needs it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Phase {
        Offline { last: Option<Ended> },
        /// Backoff between attempts. `until` is checked on each Tick.
        Waiting { attempt: u32, until: Instant, last: Ended },
        Connecting { conn: ConnectionId, attempt: u32, server: ServerAddressId, since: Instant },
        Registering { conn: ConnectionId, attempt: u32, server: ServerAddressId, reg: Registration },
        Online {
            conn: ConnectionId,
            server: ServerAddressId,
            since: Instant,
            last_rx: Instant,
            ping_sent: Option<Instant>,
            autojoin: Autojoin,
        },
        /// QUIT sent, waiting for Closed or the 2 s force.
        Quitting { conn: ConnectionId, since: Instant },
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Autojoin {
        Done,
        /// autojoinWaitsForNickServ and SASL did not log us in.
        WaitingForNickServ { warn_at: Instant },
        At(Instant),
    }

    /// Whether the user wants this network connected. Separate from Phase so
    /// "disconnected because the user said so" and "disconnected and
    /// retrying" cannot be confused.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Desired {
        Online,
        Offline,
    }

    pub struct Session {
        pub config: NetworkConfig,
        pub desired: Desired,
        pub phase: Phase,
        pub next_conn: u32,
        pub next_server: usize,
        pub isupport: ISupport,
        pub casemap: CaseMapping,
        pub nick: String,
        /// Learned from WHO/JOIN for the split budget. None means guess 60.
        pub own_hostmask: Option<String>,
        pub away: bool,
        pub channels: HashMap<ChanKey, Channel>,
        pub queries: HashMap<NickKey, Query>,
        /// Buffer order for the sidebar: channels first, then queries, in
        /// insertion order. Derived from `channels`/`queries` plus this list.
        pub buffer_order: Vec<BufferId>,
        pub buffers: HashMap<BufferId, BufferKind>,
        /// Open BATCHes: reference tag to buffered lines.
        pub batches: HashMap<String, Vec<Message>>,
        pub last_message_time: Option<String>,
    }

    // ---- inputs ----------------------------------------------------------

    /// What the user did in the UI. Every variant is one reducer step.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum UserAction {
        Connect,
        Disconnect,
        Quit { reason: Option<String> },
        /// New config for this network; diffs against the current one and
        /// applies what can change live (nick, channel list, autojoin).
        Configure(NetworkConfig),
        Join { name: String, key: Option<Secret> },
        Part { buffer: BufferId, reason: Option<String> },
        /// Close a buffer (part if joined; forget a query).
        CloseBuffer(BufferId),
        OpenQuery { nick: String },
    }

    #[derive(Debug, Clone, PartialEq)]
    pub enum Input {
        /// An event from the transport generation `conn`. Stale `conn` is
        /// ignored, which makes reconnect races a non-event.
        Transport { conn: ConnectionId, event: transport::Event },
        /// Text from the input field for `buffer`, possibly multi-line,
        /// possibly a slash command.
        Typed { buffer: BufferId, text: String },
        User(UserAction),
        /// Once a second from World. All deadlines are compared against `now`
        /// passed to `handle`, so the session never reads the clock itself.
        Tick,
    }

    // ---- outputs ---------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BufferChange {
        Opened { kind: BufferKind, name: String, index: usize },
        Closed,
        Renamed(String),
        /// Joined / logged in. Drives the sidebar label colour.
        Active(bool),
        Topic(Option<Topic>),
        /// Modes or member count changed. Title refresh if focused.
        Summary,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum MembersChange {
        /// After 366 or on (re)join. Replaces the list.
        Reset(Vec<Member>),
        Added(Member),
        Removed(NickKey),
        /// Mode, away, nick case, hostmask, or weight.
        Updated { key: NickKey, member: Member },
        /// A rename: the row keeps its position logic in the sorter.
        Renamed { from: NickKey, member: Member },
    }

    /// Snapshot of Phase for the sidebar row and the title. Derived.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Status {
        Offline,
        Reconnecting { attempt: u32, in_secs: u64 },
        Connecting,
        Registering,
        Online { nick: String, network: String, away: bool, secured: bool },
        Quitting,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Persist {
        LastMessageTime(String),
        /// The channel list changed (join/part/close): config.toml needs a
        /// save at the next explicit point.
        ChannelList(Vec<ChannelConfig>),
    }

    #[derive(Debug, Clone, PartialEq)]
    pub enum Effect {
        Connect { conn: ConnectionId, endpoint: Endpoint },
        Send(Message),
        Close,
        Print { buffer: BufferId, line: Line },
        Buffer(BufferId, BufferChange),
        Members(BufferId, MembersChange),
        Status(Status),
        Persist(Persist),
        /// Something only the window can do: focus a buffer after /join or
        /// /query, show the flood confirmation.
        Ui(UiRequest),
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum UiRequest {
        Focus(BufferId),
        /// More than 4 lines or 4x510 bytes typed. The window asks and, on
        /// yes, dispatches `Input::Typed` again with `confirmed`.
        ConfirmFlood { buffer: BufferId, text: String, lines: usize },
    }

    // ---- the reducer -----------------------------------------------------

    impl Session {
        /// Buffers for every configured channel exist from the start, parted.
        pub fn new(config: NetworkConfig) -> Self {
            todo!()
        }

        /// The one entry point. Pure apart from `now`: no I/O, no clock, no
        /// GTK. Deterministic given the same inputs and instants.
        pub fn handle(&mut self, input: Input, now: Instant) -> Vec<Effect> {
            todo!()
        }

        pub fn status(&self, now: Instant) -> Status {
            todo!()
        }
        pub fn console(&self) -> BufferId {
            BufferId(self.config.id.0)
        }
        pub fn channel(&self, id: BufferId) -> Option<&Channel> {
            todo!()
        }
        pub fn query(&self, id: BufferId) -> Option<&Query> {
            todo!()
        }
        /// Everything the title and the member list need to draw `buffer`.
        pub fn summary(&self, buffer: BufferId) -> BufferSummary {
            todo!()
        }
        /// Candidates for Tab completion in `buffer`: members ordered by
        /// weight (decayed at `now`), rank, nick; channels; commands.
        pub fn completion_source(&self, buffer: BufferId, now: Instant) -> compose::Candidates {
            todo!()
        }

        // ---- private steps, one file each under src/session/ --------------

        /// connection.rs: Connect/Close/Tick deadlines, backoff, server
        /// round-robin, registration watchdog, ping idle, quit force.
        fn step_lifecycle(&mut self, input: &Input, now: Instant) -> Vec<Effect> {
            todo!()
        }
        /// registration.rs: CAP LS 302, REQ, SASL, NICK collisions, 001,
        /// 005, on-connect commands, autojoin scheduling.
        fn step_registration(&mut self, message: &Message, now: Instant) -> Vec<Effect> {
            todo!()
        }
        /// membership.rs: JOIN PART QUIT KICK KILL NICK MODE AWAY CHGHOST
        /// ACCOUNT 353 366 352 324 332 333 TOPIC.
        fn step_membership(&mut self, message: &Message, now: Instant) -> Vec<Effect> {
            todo!()
        }
        /// print.rs: Line::from_message, destination to BufferId (queries
        /// auto-open, console fallback, drop for channels we are not in),
        /// BATCH buffering, server-time, lastMessageTime.
        fn step_print(&mut self, message: &Message, now: Instant) -> Vec<Effect> {
            todo!()
        }
        /// typed.rs: compose::parse, local echo unless echo-message, Send per
        /// chunk, slash command table.
        fn step_typed(&mut self, buffer: BufferId, text: String, now: Instant) -> Vec<Effect> {
            todo!()
        }
        /// Re-keys channels, queries and members when 005 changes CASEMAPPING.
        fn rekey(&mut self, mapping: CaseMapping) {
            todo!()
        }
        /// Everything that resets when a connection ends: isupport, caps,
        /// members, statuses to Parted. Buffers stay.
        fn reset_connection_state(&mut self) -> Vec<Effect> {
            todo!()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct BufferSummary {
        pub kind: BufferKind,
        pub name: String,
        pub member_count: usize,
        pub modes: String,
        pub topic: Option<Topic>,
        pub hostmask: Option<String>,
    }

    /// Backoff policy. Pure, so the schedule is a table test.
    pub fn backoff(attempt: u32) -> Duration {
        // TODO 5, 10, 20, 40, 80, 160, 300 s cap, plus up to 20 percent jitter
        // seeded from attempt so tests are deterministic.
        todo!()
    }

    /// Member ordering used by the UI sorter and by completion ties:
    /// highest prefix rank first, then nick under the casemapping.
    pub fn compare_members(isupport: &ISupport, casemap: CaseMapping, a: &Member, b: &Member) -> std::cmp::Ordering {
        todo!()
    }
}
use session::*;

// ---------------------------------------------------------------------------
// compose: typed text to wire lines, history, completion. Pure.
// ---------------------------------------------------------------------------
pub mod compose {
    use super::*;

    pub const MAX_BODY: usize = 510;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Parsed {
        /// Text (or `//text`, or a lone `/`) for the buffer's target.
        Say(String),
        Command(SlashCommand),
        /// Console: every line is raw, slash or not.
        Raw(String),
    }

    /// Phase-2 command set. Anything else is sent raw as `UPPER rest`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SlashCommand {
        Join { channels: Vec<String>, keys: Vec<String> },
        Part { channel: Option<String>, reason: Option<String> },
        Msg { target: String, text: String },
        Query { nick: String, text: Option<String> },
        Me(String),
        Notice { target: String, text: String },
        Nick(String),
        Topic { channel: Option<String>, text: Option<String> },
        Away(Option<String>),
        Whois(String),
        Mode { target: Option<String>, modes: String },
        Kick { channel: Option<String>, nick: String, reason: Option<String> },
        Invite { nick: String, channel: Option<String> },
        Connect,
        Disconnect,
        Quit(Option<String>),
        Server { host: String, port: Option<u16>, tls: bool },
        Close,
        Clear,
        Raw(String),
        Unknown { name: String, rest: String },
    }

    /// One line of input, in the context of the buffer it was typed in.
    pub fn parse(line: &str, kind: &BufferKind) -> Parsed {
        todo!()
    }

    /// Facts the splitter needs. 60 characters stands in for an unknown
    /// hostmask, like Textual.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SendContext<'a> {
        pub own_hostmask: Option<&'a str>,
        pub target: &'a str,
        pub command: &'a str,
    }

    /// Splits `text` into chunks that fit `MAX_BODY` minus
    /// `:hostmask CMD target :` minus CRLF, breaking on whitespace within
    /// the last 25 bytes when possible, never inside a UTF-8 sequence, and
    /// re-opening formatting codes at the start of each chunk.
    pub fn split(text: &str, ctx: &SendContext<'_>) -> Vec<String> {
        todo!()
    }

    /// Textual's flood prompt threshold: more than 4 lines or 4 x 510 bytes.
    pub fn needs_flood_confirmation(text: &str) -> bool {
        todo!()
    }

    // ---- history ---------------------------------------------------------

    /// Textual's TLOInputHistory, per buffer or global per the preference.
    /// 100 entries. Unlike Textual, consecutive duplicates are not stored.
    pub struct History {
        per_buffer: bool,
        buffers: HashMap<BufferRef, HistoryBuffer>,
        global: HistoryBuffer,
    }

    #[derive(Default)]
    pub struct HistoryBuffer {
        entries: VecDeque<String>,
        position: usize,
        draft: Option<String>,
    }

    impl History {
        pub fn new(per_buffer: bool) -> Self {
            todo!()
        }
        pub fn add(&mut self, buffer: BufferRef, text: &str) {
            todo!()
        }
        /// Up. `current` is the field text, pushed as the draft on first move.
        pub fn previous(&mut self, buffer: BufferRef, current: &str) -> Option<String> {
            todo!()
        }
        pub fn next(&mut self, buffer: BufferRef, current: &str) -> Option<String> {
            todo!()
        }
        /// Focus moved: swap drafts when per-buffer. Returns the text to put
        /// in the field.
        pub fn move_focus(&mut self, from: Option<BufferRef>, to: BufferRef, field: &str) -> String {
            todo!()
        }
        pub fn forget(&mut self, buffer: BufferRef) {
            todo!()
        }
    }

    // ---- completion ------------------------------------------------------

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FieldSnapshot {
        pub text: String,
        /// Byte offsets.
        pub caret: usize,
        pub selection: Option<(usize, usize)>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Edit {
        pub replace: (usize, usize),
        pub with: String,
        pub caret: usize,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Direction {
        Forward,
        Backward,
    }

    /// What `Session::completion_source` hands over. Already ordered.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Candidates {
        /// Only in channels; empty elsewhere, which makes nick completion a
        /// no-op in queries and the console like Textual.
        pub nicks: Vec<String>,
        /// Current channel first, then the network's others in list order.
        pub channels: Vec<String>,
        pub commands: Vec<&'static str>,
        pub network_name: Option<String>,
    }

    /// TLONicknameCompletionStatus. Holds the last scan so repeated Tabs
    /// cycle; any other edit of the field invalidates it implicitly.
    #[derive(Default)]
    pub struct Completion {
        last: Option<CompletionScan>,
        pub suffix: String,
    }

    struct CompletionScan {
        pattern_range: (usize, usize),
        matches: Vec<String>,
        index: usize,
        left_text: String,
        left_caret: usize,
        prefix: char,
    }

    impl Completion {
        pub fn step(&mut self, field: &FieldSnapshot, dir: Direction, from: &Candidates) -> Option<Edit> {
            todo!()
        }
        pub fn reset(&mut self) {
            todo!()
        }
    }
}

// ---------------------------------------------------------------------------
// ui: GTK projection. Signatures only; gtk types are named in comments
// because this sketch crate does not link GTK.
// ---------------------------------------------------------------------------
pub mod ui {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    pub struct Clock;
    impl Clock {
        pub fn now(&self) -> Instant {
            todo!()
        }
    }

    /// One network's live pieces. The Session is the only state; the rest
    /// is plumbing to tokio and to WebKit.
    pub struct Slot {
        pub session: Session,
        /// Present between Effect::Connect and transport::Event::Closed.
        pub transport: Option<tokio::sync::mpsc::UnboundedSender<transport::Command>>,
        /// One WebKit view per buffer, created on first Print or first focus.
        pub views: HashMap<BufferId, ChatView>,
    }
    pub struct ChatView; // src/ui/chat_view.rs, unchanged

    pub struct WorldInner {
        pub slots: HashMap<NetworkId, Slot>,
    }

    /// The only object that sees both sessions and widgets. Lives on the
    /// main thread; GTK callbacks hold a clone.
    #[derive(Clone)]
    pub struct World {
        inner: Rc<RefCell<WorldInner>>,
        // sidebar: Sidebar, member_list: MemberList, window: MainWindow,
        // style: Rc<Style>, state_store: StateStore, clock: Clock
    }

    impl World {
        /// Builds a Session per configured network, the sidebar rows, and
        /// dispatches UserAction::Connect for auto_connect networks staggered
        /// by one second.
        pub fn start(config: config::Config, state: config::RuntimeState, runtime: tokio::runtime::Handle) -> World {
            todo!()
        }
        /// Borrow, reduce, release, then apply. See the module doc.
        pub fn dispatch(&self, net: NetworkId, input: Input) {
            todo!()
        }
        fn apply(&self, net: NetworkId, effect: Effect) {
            todo!()
        }
        /// Spawns transport::spawn, and a glib local future that forwards
        /// each event as Input::Transport { conn, event }.
        fn open_transport(&self, net: NetworkId, conn: ConnectionId, endpoint: Endpoint) {
            todo!()
        }
        /// Read access for completion, the title and the member list rebuild.
        pub fn with_session<R>(&self, net: NetworkId, f: impl FnOnce(&Session) -> R) -> R {
            todo!()
        }
        /// Config dialog result: diff per network, Configure live ones,
        /// create or destroy slots for added or removed networks, save.
        pub fn replace_config(&self, config: config::Config) {
            todo!()
        }
        /// Close request: Quit to every online session, then destroy the
        /// window when all are Offline or after 3 s.
        pub fn shutdown(&self, done: impl FnOnce() + 'static) {
            todo!()
        }
    }

    /// Sidebar: `gtk::ListView` over a `gtk::TreeListModel` whose roots are
    /// `NetworkRow` GObjects (child model: `gio::ListStore` of `BufferRow`).
    /// `gtk::SingleSelection` gives the focused item. Rows carry CSS classes
    /// `server-row`, `channel-row`, `query-row`, `active`, `unread`,
    /// `highlight`, `selected` for the Mac stylesheet.
    pub struct Sidebar;
    /// GObject with properties: id (BufferRef), name, kind, active,
    /// unread (u32), highlights (u32). Properties are the only writers of the
    /// row widget; the Session never sees these counters.
    pub struct BufferRow;
    /// GObject with properties: id, name, status text, connected, expanded.
    pub struct NetworkRow;

    impl Sidebar {
        pub fn add_network(&self, net: NetworkId, name: &str, expanded: bool) {
            todo!()
        }
        pub fn apply_buffer(&self, net: NetworkId, id: BufferId, change: BufferChange) {
            todo!()
        }
        pub fn set_status(&self, net: NetworkId, status: &Status) {
            todo!()
        }
        /// Bumps unread (and highlights when `line.highlight`) unless the
        /// buffer is focused and the window is active.
        pub fn count(&self, target: BufferRef, line: &Line, focused: Option<BufferRef>, window_active: bool) {
            todo!()
        }
        pub fn mark_read(&self, target: BufferRef) {
            todo!()
        }
        pub fn focused(&self) -> Option<BufferRef> {
            todo!()
        }
        pub fn focus(&self, target: BufferRef) {
            todo!()
        }
        /// Textual's navigation: next/previous channel, active, unread,
        /// server; Cmd+1..0 across networks. Pure over the row list.
        pub fn navigate(&self, how: Navigate) -> Option<BufferRef> {
            todo!()
        }
        pub fn probe_json(&self) -> String {
            todo!()
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Navigate {
        Next,
        Previous,
        NextActive,
        PreviousActive,
        NextUnread,
        PreviousUnread,
        NextNetwork,
        PreviousNetwork,
        Nth(usize),
    }

    /// Member list: one `gio::ListStore<MemberRow>` under a
    /// `gtk::SortListModel` with a `CustomSorter` built from
    /// `session::compare_members`. Rebuilt from the Session when focus
    /// moves to a channel; then only `MembersChange` for that channel apply.
    pub struct MemberList;
    pub struct MemberRow;

    impl MemberList {
        pub fn show(&self, target: Option<BufferRef>, members: Vec<Member>, isupport: &ISupport, casemap: CaseMapping) {
            todo!()
        }
        /// Ignored unless `target` is the shown channel.
        pub fn apply(&self, target: BufferRef, change: MembersChange) {
            todo!()
        }
        pub fn probe_json(&self) -> String {
            todo!()
        }
    }

    /// Input bar: a growing `gtk::TextView` (1 to 6 lines). Return sends,
    /// Shift+Return inserts a newline, Up/Down walk history when the caret is
    /// on the first/last line, Tab/Shift+Tab complete. CSS class `input-bar`.
    pub struct InputBar;
    impl InputBar {
        pub fn snapshot(&self) -> compose::FieldSnapshot {
            todo!()
        }
        pub fn apply(&self, edit: compose::Edit) {
            todo!()
        }
        pub fn take_text(&self) -> String {
            todo!()
        }
        pub fn set_text(&self, text: &str) {
            todo!()
        }
    }

    /// Title: Textual's `updateTitle`, as a pure function the window calls
    /// on Effect::Status, BufferChange::Summary and focus change.
    pub fn title(status: &Status, focused: Option<&BufferSummary>) -> (String, String) {
        todo!()
    }

    /// Throttled writer for RuntimeState: coalesces Persist effects and
    /// writes at most every 300 s, plus at quit.
    pub struct StateStore;
    impl StateStore {
        pub fn record(&self, net: NetworkId, change: Persist) {
            todo!()
        }
        pub fn flush(&self) {
            todo!()
        }
    }
}

// ---------------------------------------------------------------------------
// tests/support/script.rs: the lever for reducer tests.
// ---------------------------------------------------------------------------
pub mod script {
    use super::*;

    /// Drives one Session with a fake clock and collects effects. A `.irc`
    /// fixture is a list of lines: `S: <line>` feeds a server line, `C: <line>`
    /// asserts the next Send, `T: +30s` advances the clock and ticks,
    /// `U: connect` and friends dispatch UserActions, `E: print #chan ...`
    /// asserts a Print. Unasserted Sends fail the run, so a test cannot pass
    /// while the client talks unexpectedly.
    pub struct Script<'a> {
        session: &'a mut Session,
        now: Instant,
        pending: VecDeque<Effect>,
    }

    impl<'a> Script<'a> {
        pub fn new(session: &'a mut Session) -> Self {
            todo!()
        }
        pub fn run_fixture(&mut self, path: &std::path::Path) {
            todo!()
        }
        pub fn server(&mut self, line: &str) {
            todo!()
        }
        pub fn transport(&mut self, event: transport::Event) {
            todo!()
        }
        pub fn user(&mut self, action: UserAction) {
            todo!()
        }
        pub fn typed(&mut self, buffer: BufferId, text: &str) {
            todo!()
        }
        pub fn advance(&mut self, by: Duration) {
            todo!()
        }
        pub fn expect_sent(&mut self, lines: &[&str]) {
            todo!()
        }
        pub fn expect_connect(&mut self) -> ConnectionId {
            todo!()
        }
        pub fn expect_close(&mut self) {
            todo!()
        }
        pub fn expect_print(&mut self, buffer: BufferId, body_contains: &str) -> Line {
            todo!()
        }
        pub fn drain(&mut self) -> Vec<Effect> {
            todo!()
        }
    }
}

fn main() {}

// Keep otherwise-unused imports honest for the sketch.
fn _touch(_: HashSet<u8>, _: PathBuf, _: ModeChange, _: CapStep, _: ChannelConfig) {}
