//! Candidate 3: sans-IO core, one owner on the GTK thread, TEA-style loop.
//!
//! Everything in this file belongs to `textual-core` (no gtk, glib, tokio).
//! Stubs for phase-1 types are marked `// existing`. Bodies are `todo!()`.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::{Instant, SystemTime};

// ───────────────────────── ids ─────────────────────────

/// A UUID underneath, branded so a server id cannot stand in for a buffer id.
pub struct Id<T>(pub u128, PhantomData<fn() -> T>);
macro_rules! id_impls {
    ($t:ident) => {
        impl<T> Clone for $t<T> { fn clone(&self) -> Self { *self } }
        impl<T> Copy for $t<T> {}
        impl<T> PartialEq for $t<T> { fn eq(&self, o: &Self) -> bool { self.0 == o.0 } }
        impl<T> Eq for $t<T> {}
        impl<T> std::hash::Hash for $t<T> { fn hash<H: std::hash::Hasher>(&self, h: &mut H) { self.0.hash(h) } }
        impl<T> std::fmt::Debug for $t<T> { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{:032x}", self.0) } }
    };
}
id_impls!(Id);
pub enum ServerTag {}
pub enum BufferTag {}
pub type ServerId = Id<ServerTag>;
pub type BufferId = Id<BufferTag>;

/// One socket's lifetime. Bumped on every dial, so late events from a
/// dead socket are recognisably stale.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct ConnId(pub u64);

// ───────────────────── irc: casemapping ─────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Casemap { Ascii, #[default] Rfc1459, Rfc1459Strict }

/// A nick or channel name folded under one casemapping. The only way to
/// make one is `Casemap::fold`, so a map keyed by `Folded` cannot be
/// looked up with an unfolded string.
#[derive(Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct Folded(Box<str>);

impl Casemap {
    pub fn fold(self, s: &str) -> Folded { todo!() }
    pub fn eq(self, a: &str, b: &str) -> bool { todo!() }
    /// `CASEMAPPING=` token value; unknown values keep the current mapping.
    pub fn from_token(v: &str) -> Option<Casemap> { todo!() }
}

// ───────────────────── irc: wire ─────────────────────

pub struct Message { pub tags: Vec<(String, String)>, pub source: Option<String>, pub command: String, pub params: Vec<String> } // existing
pub struct Line { /* existing: id, received_at, kind, command, nickname, from_self, body, highlight */ } // existing

/// A line already checked against the grammar and the 512-byte budget
/// (tags excluded). The transport accepts nothing else, so "it was sent"
/// and "it was valid" are one fact.
#[derive(Clone, Debug)]
pub struct WireLine(String);
#[derive(Debug)]
pub enum WireError { TooLong { bytes: usize }, Forbidden(char), Empty }
impl WireLine {
    pub fn new(m: &Message) -> Result<WireLine, WireError> { todo!() }
    pub fn as_str(&self) -> &str { &self.0 }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Priority { Normal, Immediate /* PONG, QUIT */ }

// ───────────────────── config (schema, GTK-free) ─────────────────────

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub id: ServerId,
    pub name: String,
    pub addresses: Vec<ServerAddress>,
    pub nick: String,
    pub alt_nicks: Vec<String>,
    pub username: String,
    pub real_name: String,
    pub auto_connect: bool,
    pub auto_reconnect: bool,
    pub autojoin_waits_for_nickserv: bool,
    pub on_connect: Vec<String>,
    pub verify_chain: bool,
    pub trusted_fingerprints: Vec<String>,
    pub sidebar_expanded: bool,
    pub channels: Vec<ChannelConfig>,
    // encodings, proxy, flood, client_cert, highlights, ignores: same shape, omitted here
}
#[derive(Clone, Debug)]
pub struct ServerAddress { pub id: u128, pub host: String, pub port: u16, pub tls: bool }
#[derive(Clone, Debug)]
pub struct ChannelConfig { pub id: BufferId, pub name: String, pub auto_join: bool, pub default_modes: Option<String>, pub default_topic: Option<String> }

/// What a secret belongs to; the libsecret attribute set is derived from it.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum SecretRef { ServerPassword(u128), NickServ(ServerId), ChannelKey(BufferId), Proxy(ServerId) }
/// No `Debug` that prints the value, no `Display`, no serde.
pub struct Secret(String);
#[derive(Default)]
pub struct SecretCache(HashMap<SecretRef, Secret>);
impl SecretCache { pub fn get(&self, r: &SecretRef) -> Option<&Secret> { self.0.get(r) } }

/// Runtime state persisted separately from config (own file, throttled).
#[derive(Default, Clone, Debug)]
pub struct SavedState { pub selection: Option<BufferId>, pub last_server_time: HashMap<ServerId, SystemTime>, pub window: (i32, i32) }

// ───────────────────── time ─────────────────────

/// Passed in, never read inside the core, so tests own the clock.
#[derive(Clone, Copy)]
pub struct Clock { pub mono: Instant, pub wall: SystemTime }

// ───────────────────── session: lifecycle ─────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Disconnect {
    UserQuit,
    ServerClosed,
    Io(String),
    TimedOut,
    RegistrationFailed(RegistrationFailure),
    ServerError(String),
    BadCertificate { sha256: String },
}
#[derive(Debug, Clone, PartialEq)]
pub enum RegistrationFailure { Banned, BadPassword, NickUnavailable, Watchdog }

impl Disconnect {
    /// Policy, not mechanism: which endings are worth another try.
    pub fn retryable(&self) -> bool { todo!() }
}

pub enum Phase {
    /// No socket. `retry` is `None` when nothing will redial by itself.
    Idle { why: Option<Disconnect>, retry: Option<Retry> },
    /// Dial asked for; the transport has not answered.
    Connecting(Link),
    /// Socket up, before 001. CAP, SASL and nick negotiation live here.
    Registering(Link, Registration),
    Online(Link, Online),
    /// QUIT sent; wait for the server's ERROR until `until`, then hang up.
    Quitting(Link, Instant),
}
pub struct Retry { pub attempt: u32, pub at: Instant }
pub struct Link { pub conn: ConnId, pub address_ix: usize, pub opened: Option<Instant>, pub last_rx: Instant, pub pinged: bool }
pub struct Online { pub since: Instant, pub autojoin: AutojoinGate }
pub enum AutojoinGate { Open, AwaitingNickServ { deadline: Instant }, Done }

/// CAP LS 302 → REQ → (SASL) → CAP END → 001. Pure; see scenario tests.
pub struct Registration { /* caps offered, requested, sasl state, attempted nick */ }
pub enum RegProgress { Continue, Welcome { nick: String }, Failed(RegistrationFailure) }
impl Registration {
    pub fn on_message(&mut self, m: &Message, cfg: &ServerConfig, secrets: &SecretCache, send: &mut Vec<Message>) -> RegProgress { todo!() }
}

// ───────────────────── session: buffers ─────────────────────

pub enum Body {
    Console,
    Channel(ChannelState),
    Query(QueryState),
}
pub struct Buffer {
    pub id: BufferId,
    pub name: String,
    pub key: Folded,
    pub body: Body,
    pub counters: Counters,
    pub scrollback: Scrollback,
    pub input: InputMemory,
}
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Counters { pub unread: u32, pub highlights: u32 }
/// Capped log of printed lines. The chat view is a cache of this, so a view
/// can be dropped and rebuilt without losing anything.
pub struct Scrollback { lines: VecDeque<Rc<Line>>, cap: usize }

pub enum JoinState { Parted, Joining, Joined, Failed(u16) }
pub struct ChannelState {
    pub join: JoinState,
    pub topic: Option<Topic>,
    pub modes: String,
    pub members: Members,
    /// 353 lines accumulate here; 366 swaps them in whole, so NAMES can run again.
    pub names_in_flight: Option<Vec<Member>>,
}
pub struct QueryState { pub peer: Folded }
pub struct Topic { pub text: String, pub set_by: Option<String>, pub at: Option<SystemTime> }

#[derive(Clone, Debug)]
pub struct Member { pub nick: String, pub key: Folded, pub modes: String, pub rank: u8, pub weight: f32 }

/// What happened to the list, in the shape `gio::ListStore::splice` wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberOp { Reset, Insert(usize), Remove(usize), Replace(usize) }

/// Sorted by (rank desc, folded nick). The sort is the only place order is decided.
#[derive(Default)]
pub struct Members { sorted: Vec<Member> }
impl Members {
    pub fn len(&self) -> usize { self.sorted.len() }
    pub fn get(&self, ix: usize) -> Option<&Member> { self.sorted.get(ix) }
    pub fn find(&self, key: &Folded) -> Option<usize> { todo!() }
    pub fn insert(&mut self, m: Member, ops: &mut Vec<MemberOp>) { todo!() }
    pub fn remove(&mut self, key: &Folded, ops: &mut Vec<MemberOp>) -> Option<Member> { todo!() }
    pub fn rename(&mut self, from: &Folded, to: String, to_key: Folded, ops: &mut Vec<MemberOp>) { todo!() }
    pub fn set_modes(&mut self, key: &Folded, modes: String, rank: u8, ops: &mut Vec<MemberOp>) { todo!() }
    pub fn replace_all(&mut self, all: Vec<Member>, ops: &mut Vec<MemberOp>) { todo!() }
}

// ───────────────────── session ─────────────────────

/// One server's protocol state and its buffers. Pure: every method takes the
/// clock and appends to `Out`; nothing here sleeps, reads a socket or draws.
pub struct Session {
    pub config: Rc<ServerConfig>,
    phase: Phase,
    next_conn: u64,
    casemap: Casemap,
    me: String,
    caps: EnabledCaps,
    isupport: Isupport,
    buffers: Vec<Buffer>,         // console first, channels, then queries
    later: BTreeMap<(Instant, u64), Later>,
}
#[derive(Default)] pub struct EnabledCaps { /* bitset */ }
#[derive(Default)] pub struct Isupport { /* prefix, chanmodes, chantypes, network, nicklen ... */ }
pub enum Later { Rejoin(BufferId), NickServGiveUp }

pub enum SessionInput {
    Connect,                              // user or autoconnect
    Quit(Option<String>),
    Net(ConnId, NetEvent),
    Wake,
    ConfigChanged(Rc<ServerConfig>),
    Network { online: bool },
    Submit { buffer: BufferId, line: OutgoingLine },
    JoinChannel(String),
    CloseBuffer(BufferId),
}
/// A line of user input, already classified by `input::command::parse`.
pub enum OutgoingLine { Say(String), Action(String), Notice(String), Command { name: String, args: String } }

/// What the transport reports. Parsed off the main thread.
pub enum NetEvent {
    Opened { tls: Option<TlsInfo> },
    Line { raw: String, message: Result<Message, ()> },
    Closed(Disconnect),
}
pub struct TlsInfo { pub sha256: String, pub protocol: String }

impl Session {
    pub fn new(config: Rc<ServerConfig>, restored: &SavedState) -> Session { todo!() }
    pub fn handle(&mut self, input: SessionInput, ctx: &Ctx, out: &mut Out) { todo!() }
    /// Derived from `phase` and `later`; the loop never schedules or cancels.
    pub fn next_wake(&self) -> Option<Instant> { todo!() }
    pub fn buffers(&self) -> &[Buffer] { &self.buffers }
    pub fn buffer(&self, id: BufferId) -> Option<&Buffer> { todo!() }
    pub fn my_nick(&self) -> &str { &self.me }
    pub fn phase_view(&self) -> PhaseView { todo!() }
}
/// What the UI may know about the lifecycle, without the transitions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhaseView { Offline, Reconnecting { attempt: u32 }, Connecting, Registering, Online, Quitting }

pub struct Ctx<'a> {
    pub clock: Clock,
    pub secrets: &'a SecretCache,
    pub focus: Focus,
    pub server: ServerId,
}
/// Whether the user can see a buffer right now: decides whether a new line counts as unread.
#[derive(Clone, Copy)]
pub struct Focus { pub buffer: Option<BufferId>, pub window_active: bool }

// ───────────────────── out: effects and changes ─────────────────────

#[derive(Default)]
pub struct Out { pub effects: Vec<Effect>, pub changes: Vec<Change> }

/// Things that must happen in the world. Executed by the app's `Io`.
pub enum Effect {
    Dial { server: ServerId, conn: ConnId, address: ServerAddress, tls: TlsPolicy },
    Send { server: ServerId, conn: ConnId, line: WireLine, priority: Priority },
    Hangup { server: ServerId, conn: ConnId },
    SaveConfig,
    SaveState,
    Quit, // every session is down; the window may close
}
pub struct TlsPolicy { pub verify_chain: bool, pub pinned: Vec<String> }

/// Things that changed in the model. Ids and the few payloads that cannot be
/// pulled; sinks read the rest from `&Workspace`.
pub enum Change {
    ServerAdded(ServerId),
    ServerRemoved(ServerId),
    ServerPhase(ServerId),
    BufferAdded(BufferId),
    BufferRemoved(BufferId),
    BufferChanged(BufferId),
    Topic(BufferId),
    Line { buffer: BufferId, line: Rc<Line>, attention: Attention },
    Members { buffer: BufferId, ops: Vec<MemberOp> },
    Counters(BufferId),
    Selection,
    SetInput { text: String, caret: usize },
    ClearInput,
    Ask(Prompt),
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Attention { None, Unread, Highlight }
pub enum Prompt { Flood { buffer: BufferId, lines: usize }, TrustCertificate { server: ServerId, sha256: String } }

/// Every consumer of changes: the window, persistence, later notifications and logging.
pub trait Sink {
    fn apply(&mut self, ws: &Workspace, changes: &[Change]);
}

// ───────────────────── workspace ─────────────────────

pub enum Msg {
    Net(ServerId, ConnId, NetEvent),
    Ui(UiAction),
    SecretsLoaded(Result<Vec<(SecretRef, Secret)>, String>),
    Wake,
    Network { online: bool },
    Suspend,
    Resume,
}

pub enum UiAction {
    Select { buffer: BufferId, leaving_draft: String },
    SelectOffset { nav: Nav, leaving_draft: String },
    Submit { buffer: BufferId, text: String, confirmed: bool },
    Complete { text: String, caret: usize, backwards: bool },
    History { dir: HistoryDir, text: String },
    Connect(ServerId),
    Disconnect(ServerId),
    CloseBuffer(BufferId),
    SetExpanded(ServerId, bool),
    EditServer(ServerConfig),
    RemoveServer(ServerId),
    WindowActive(bool),
    MarkAllRead,
}
#[derive(Clone, Copy)] pub enum HistoryDir { Older, Newer }
#[derive(Clone, Copy)] pub struct Nav { pub step: i32, pub kind: NavKind, pub filter: NavFilter, pub within_server: bool }
#[derive(Clone, Copy)] pub enum NavKind { Channels, Servers, Any }
#[derive(Clone, Copy)] pub enum NavFilter { All, Active, Unread, Highlight }

#[derive(Clone, Copy, Default)]
pub struct Selection { pub focused: Option<BufferId>, pub previous: Option<BufferId> }

/// The single owner of everything the user can see or change.
pub struct Workspace {
    servers: Vec<Session>,
    selection: Selection,
    window_active: bool,
    secrets: SecretCache,
    secrets_loaded: bool,
    completion: Option<LastCompletion>,
    prefs: Prefs,
}
pub struct Prefs { pub history_scope: HistoryScope, pub scrollback: usize }
#[derive(Clone, Copy)] pub enum HistoryScope { PerBuffer, Global }

impl Workspace {
    pub fn new(servers: Vec<ServerConfig>, state: SavedState, prefs: Prefs) -> Workspace { todo!() }
    pub fn update(&mut self, msg: Msg, clock: Clock, out: &mut Out) { todo!() }
    pub fn next_wake(&self) -> Option<Instant> { todo!() }

    pub fn servers(&self) -> impl Iterator<Item = &Session> { self.servers.iter() }
    pub fn selection(&self) -> Selection { self.selection }
    pub fn buffer(&self, id: BufferId) -> Option<(&Session, &Buffer)> { todo!() }
    /// Window title and subtitle for a buffer, derived on demand.
    pub fn title(&self, id: BufferId) -> Title { todo!() }
    /// Cmd+1..0: channels across every server in sidebar order.
    pub fn nth_channel(&self, n: usize) -> Option<BufferId> { todo!() }
    pub fn to_config(&self) -> Vec<ServerConfig> { todo!() }
    pub fn to_state(&self) -> SavedState { todo!() }
}
pub struct Title { pub primary: String, pub secondary: String }

// ───────────────────── input (pure) ─────────────────────

pub struct InputMemory { pub history: History, pub draft: String }
/// At most 100 entries, no consecutive duplicates, the unsent draft kept apart.
pub struct History { items: VecDeque<String>, cursor: Option<usize>, stash: Option<String> }
impl History {
    pub fn push(&mut self, line: &str) { todo!() }
    /// `None` means "leave the field alone" (already at the oldest entry).
    pub fn step(&mut self, dir: HistoryDir, current: &str) -> Option<String> { todo!() }
    pub fn reset_cursor(&mut self) { todo!() }
}

/// What the last Tab left behind; reused only while text and caret still match it.
pub struct LastCompletion { text: String, caret: usize, candidates: Vec<String>, ix: usize, replace: std::ops::Range<usize> }
pub struct CompletionSource<'a> {
    pub members: Option<&'a Members>,   // None in a query or console: no nick completion
    pub channels: Vec<&'a str>,
    pub commands: &'a [&'a str],
    pub network_name: Option<&'a str>,
    pub suffix: &'a str,
}
pub struct Completed { pub text: String, pub caret: usize, pub state: LastCompletion }
pub fn complete(src: &CompletionSource, last: Option<&LastCompletion>, text: &str, caret: usize, backwards: bool) -> Option<Completed> { todo!() }

/// Splits one logical line into wire-sized chunks. `budget` is bytes left for
/// the text after `:nick!user@host PRIVMSG target :` and CRLF. Never breaks
/// inside a UTF-8 sequence, prefers a space within the last 25 bytes, and
/// re-opens active IRC formatting at the start of the next chunk.
pub fn split_for_wire(text: &str, budget: usize) -> Vec<&str> { todo!() }

/// Slash handling that depends on where you are typing.
pub fn classify(text: &str, buffer_is_console: bool) -> Vec<OutgoingLine> { todo!() }
