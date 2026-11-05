//! The transcript runner: drives a `Workspace` holding one network through
//! a `.irc` script in virtual time.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::mem::Discriminant;
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::{Local, TimeZone};
use textual_core::irc::Message;
use textual_core::irc::caps::WANTED;
use textual_core::irc::casemap::CaseMapping;
use textual_core::line::{CLIENT_COMMAND, Line};
use textual_core::model::buffer::{Buffer, BufferId, Counters, Rejoin};
use textual_core::model::members::{MemberDiff, MemberRow};
use textual_core::model::network::{
    self, Address, CloseReason, ConnId, Effect, Ended, Link, Network, ServerConfig, ServerId, Tls,
    TransportEvent,
};
use textual_core::model::registration::Auth;
use textual_core::model::workspace::{Input, Out, UiAction, Workspace};
use textual_core::model::{Change, Now};
use textual_core::secret::Secret;

use super::setup;

pub struct Driver {
    ws: Workspace,
    server: ServerId,
    start: Instant,
    now: Instant,
    latest: Option<ConnId>,
    sent: VecDeque<String>,
    notices: VecDeque<String>,
    view: View,
    /// Where the view built from changes disagreed with the workspace.
    drift: Vec<String>,
    /// Changes withheld from the view, to prove its checks can fail.
    hide: fn(&Change) -> bool,
}

impl Driver {
    pub fn new(config: ServerConfig, hide: fn(&Change) -> bool) -> Self {
        let server = config.id;
        let start = Instant::now();
        let mut driver = Driver {
            ws: Workspace::new(),
            server,
            start,
            now: start,
            latest: None,
            sent: VecDeque::new(),
            notices: VecDeque::new(),
            view: View::default(),
            drift: Vec::new(),
            hide,
        };
        let mut out = Out::default();
        driver.ws.add_network(config, &mut out);
        driver.take(out);
        driver
    }

    fn now(&self) -> Now {
        // A fixed wall clock, so a script's lines are the same in every run.
        let wall = Local.timestamp_opt(1_800_000_000, 0).unwrap() + (self.now - self.start);
        Now {
            mono: self.now,
            wall,
        }
    }

    pub fn input(&mut self, input: Input) {
        let mut out = Out::default();
        self.ws.update(input, self.now(), &mut out);
        self.take(out);
    }

    fn take(&mut self, out: Out) {
        for (_, effect) in out.effects {
            let item = match effect {
                Effect::Dial { conn, address, tls } => {
                    self.latest = Some(conn);
                    let tls = match tls {
                        Tls::Off => "plain",
                        Tls::On { verify: true } => "tls",
                        Tls::On { verify: false } => "tls-unverified",
                    };
                    format!("dial {conn} {address} {tls}")
                }
                Effect::Send { conn, line } => {
                    let text = line.as_str().strip_suffix("\r\n").expect("CRLF");
                    if Some(conn) == self.latest {
                        text.to_owned()
                    } else {
                        format!("{conn} {text}")
                    }
                }
                Effect::Hangup { conn } => format!("hangup {conn}"),
            };
            self.sent.push_back(item);
        }
        let console = self.network().buffers().console().id();
        for change in &out.changes {
            if let Change::Line { buffer, line } = change
                && *buffer == console
                && line.command == CLIENT_COMMAND
            {
                self.notices.push_back(line.plain_text());
            }
        }
        let shown: Vec<Change> = out
            .changes
            .into_iter()
            .filter(|c| !(self.hide)(c))
            .collect();
        if let Err(e) = self.view.apply(&self.ws, &shown) {
            self.drift.push(e);
        }
    }

    fn net(&mut self, input: network::Input) {
        self.input(Input::Network {
            server: self.server,
            input,
        });
    }

    pub fn transport(&mut self, conn: ConnId, event: TransportEvent) {
        self.net(network::Input::Transport { conn, event });
    }

    pub fn advance(&mut self, by: Duration) {
        self.now += by;
        self.input(Input::Wake);
    }

    pub fn network(&self) -> &Network {
        self.ws.network(self.server).expect("the one network")
    }

    fn buffer(&self, name: &str) -> Result<&Buffer, String> {
        let buffers = self.network().buffers();
        if name == "console" {
            return Ok(buffers.console());
        }
        buffers
            .iter()
            .find(|b| !b.is_console() && b.name() == name)
            .ok_or_else(|| format!("no buffer {name:?}"))
    }

    fn label(&self, id: BufferId) -> String {
        match self.ws.buffer(id) {
            Some((_, b)) if b.is_console() => "console".to_owned(),
            Some((_, b)) => b.name().to_owned(),
            None => format!("{id}"),
        }
    }
}

/// What a view built only from `Change`s would show, following the
/// design's rule: a buffer whose member list is reset in a pass is rebuilt
/// from the final state and ignores that pass's other diffs.
#[derive(Default)]
struct View {
    buffers: HashSet<BufferId>,
    members: HashMap<BufferId, Vec<MemberRow>>,
    lines: HashMap<BufferId, Vec<Rc<Line>>>,
    /// The workspace after the last pass, to catch a fact that changed
    /// with no `Change` saying so.
    seen: Snapshot,
}

/// What the view reads from the workspace when a `Change` names it.
#[derive(Default, PartialEq)]
struct Snapshot {
    buffers: HashMap<BufferId, Facts>,
    focus: Option<BufferId>,
    expanded: HashMap<ServerId, bool>,
    links: HashMap<ServerId, Discriminant<Link>>,
}

#[derive(PartialEq)]
struct Facts {
    counters: Counters,
    name: String,
    joined: bool,
    topic: Option<String>,
    modes: Option<String>,
}

impl Snapshot {
    fn of(ws: &Workspace) -> Self {
        let buffers = ws
            .networks()
            .iter()
            .flat_map(|n| n.buffers().iter())
            .map(|b| {
                let joined = b.joined();
                let facts = Facts {
                    counters: b.counters(),
                    name: b.name().to_owned(),
                    joined: joined.is_some(),
                    topic: joined.and_then(|j| j.topic().map(str::to_owned)),
                    modes: joined.map(|j| j.modes()),
                };
                (b.id(), facts)
            })
            .collect();
        let expanded = ws
            .networks()
            .iter()
            .map(|n| (n.id(), ws.is_expanded(n.id())))
            .collect();
        let links = ws
            .networks()
            .iter()
            .map(|n| (n.id(), std::mem::discriminant(n.link())))
            .collect();
        Snapshot {
            buffers,
            focus: ws.focus(),
            expanded,
            links,
        }
    }

    /// Every fact that differs from `before` without the change that
    /// announces it.
    fn unannounced(&self, before: &Snapshot, changes: &[Change]) -> Vec<String> {
        let has = |want: &Change| changes.contains(want);
        let mut missing = Vec::new();
        if self.focus != before.focus && !self.focus.is_some_and(|f| has(&Change::Focus(f))) {
            missing.push(format!(
                "focus changed without Change::Focus ({:?} to {:?})",
                before.focus, self.focus
            ));
        }
        for (server, expanded) in &self.expanded {
            let was = before.expanded.get(server);
            if was.is_some_and(|w| w != expanded) && !has(&Change::Expanded(*server)) {
                missing.push(format!(
                    "{server:?} expanded changed without Change::Expanded"
                ));
            }
        }
        for (server, link) in &self.links {
            let was = before.links.get(server);
            if was.is_some_and(|w| w != link) && !has(&Change::Link(*server)) {
                missing.push(format!("{server:?} link changed without Change::Link"));
            }
        }
        for (id, now) in &self.buffers {
            let Some(was) = before.buffers.get(id) else {
                continue;
            };
            if now.counters != was.counters && !has(&Change::Counters(*id)) {
                missing.push(format!("{id} counters changed without Change::Counters"));
            }
            let fields = [
                ("name", now.name != was.name),
                ("joined", now.joined != was.joined),
                ("topic", now.topic != was.topic),
                ("modes", now.modes != was.modes),
            ];
            for (what, differs) in fields {
                if differs && !has(&Change::BufferChanged(*id)) {
                    missing.push(format!("{id} {what} changed without Change::BufferChanged"));
                }
            }
        }
        missing
    }
}

fn rows(ws: &Workspace, id: BufferId) -> Vec<MemberRow> {
    let Some((net, buffer)) = ws.buffer(id) else {
        return Vec::new();
    };
    let prefix = net
        .session()
        .map_or_else(Vec::new, |s| s.isupport().prefix().to_vec());
    buffer
        .joined()
        .map_or_else(Vec::new, |j| j.members().rows(&prefix).collect())
}

impl View {
    fn apply(&mut self, ws: &Workspace, changes: &[Change]) -> Result<(), String> {
        let now = Snapshot::of(ws);
        let missing = now.unannounced(&self.seen, changes);
        self.seen = now;
        if !missing.is_empty() {
            return Err(missing.join("; "));
        }
        let reset: HashSet<BufferId> = changes
            .iter()
            .filter_map(|c| match c {
                Change::Members {
                    buffer,
                    diff: MemberDiff::Reset,
                } => Some(*buffer),
                _ => None,
            })
            .collect();
        for id in &reset {
            self.members.insert(*id, rows(ws, *id));
        }
        for change in changes {
            match change {
                Change::BufferAdded(id) => {
                    if !self.buffers.insert(*id) {
                        return Err(format!("{id} added twice"));
                    }
                }
                Change::BufferRemoved(id) => {
                    self.buffers.remove(id);
                    self.members.remove(id);
                    self.lines.remove(id);
                }
                Change::Line { buffer, line } => {
                    self.lines.entry(*buffer).or_default().push(line.clone());
                }
                Change::Members { buffer, diff } if !reset.contains(buffer) => {
                    let list = self.members.entry(*buffer).or_default();
                    let fits = |at: usize, len: usize| at <= len;
                    match diff {
                        MemberDiff::Reset => unreachable!("handled above"),
                        MemberDiff::Insert { at, row } if fits(*at, list.len()) => {
                            list.insert(*at, row.clone());
                        }
                        MemberDiff::Remove { at } if *at < list.len() => {
                            list.remove(*at);
                        }
                        MemberDiff::Update { at, row } if *at < list.len() => {
                            list[*at] = row.clone();
                        }
                        MemberDiff::Move { from, to, row } if *from < list.len() => {
                            list.remove(*from);
                            if *to > list.len() {
                                return Err(format!("{buffer}: {diff:?} out of range"));
                            }
                            list.insert(*to, row.clone());
                        }
                        _ => return Err(format!("{buffer}: {diff:?} out of range")),
                    }
                }
                _ => {}
            }
        }
        let actual: HashSet<BufferId> = ws
            .networks()
            .iter()
            .flat_map(|n| n.buffers().iter().map(Buffer::id))
            .collect();
        if actual != self.buffers {
            return Err(format!(
                "buffers from changes {:?}, in the workspace {actual:?}",
                self.buffers
            ));
        }
        for id in actual {
            let want = rows(ws, id);
            let got = self.members.get(&id).cloned().unwrap_or_default();
            if got != want {
                return Err(format!(
                    "{id} members from changes {got:?}, in the workspace {want:?}"
                ));
            }
            let (_, buffer) = ws.buffer(id).expect("listed");
            let lines = self.lines.get(&id).map_or(&[][..], Vec::as_slice);
            let kept = buffer.scrollback().len();
            let tail = &lines[lines.len().saturating_sub(kept)..];
            let same = tail.len() == kept
                && tail
                    .iter()
                    .zip(buffer.scrollback().lines())
                    .all(|(a, b)| Rc::ptr_eq(a, b));
            if !same {
                return Err(format!(
                    "{id} lines from changes differ from its scrollback"
                ));
            }
        }
        Ok(())
    }
}

/// Runs one transcript and returns every mismatch, each with its line
/// number. The DSL:
///
/// - `= key value` configures the server before the first exchange: `nick`,
///   `alternates`, `user`, `realname`, `auth none|sasl-plain|nickserv`,
///   `server-password`, `account-password`, `caps` (replaces the wanted
///   list), `id` (a number, seeds the backoff jitter), `addresses`
///   (`host:port` list), `tls off|on|unverified`, `auto-reconnect on|off`.
/// - `> item`: the next thing the client did. A line sent on the latest
///   connection is written as is; on an older one it starts with `#n`.
///   `dial #n host:port plain|tls|tls-unverified` and `hangup #n`.
/// - `< line`: a server line on the latest connection.
/// - `! connect`, `! disconnect [reason]`: user input. `! opened`,
///   `! closed eof|timeout|error <text>|bad-certificate <hex>`: a transport
///   event on the latest connection; `! #n <event>` sends it on connection n,
///   where `line <raw>` is also an event.
/// - `! welcome [caps]`: connects and registers, acknowledging whichever of
///   `caps` the client asks for, with nothing checked; for scripts about
///   what happens after 001.
/// - The window: `! focus <buffer>`, `! collapse`, `! expand`,
///   `! window active|inactive`, `! close <buffer>`, `! say <buffer> text`.
///   A buffer is `console`, a channel name or a query's nick.
/// - `@ +5s`, `@ +4.613s`, `@ +250ms`: virtual time passes. `@ wake`: time
///   passes to the network's next wake.
/// - `? notice text`: the next line the client printed to the console
///   itself, as plain text.
/// - Link queries, which are also the end states (every transcript must end
///   with one, so a truncated one cannot pass): `? registering`,
///   `? connecting`, `? quitting`, `? registered nick [account]`,
///   `? waiting <duration>` (until the retry), `? offline [why]`, and
///   `? failed no-usable-nick|error <text>` (the last link ended so).
/// - Other queries: `? next-wake <duration>|none`, `? failures n`,
///   `? desired online|offline`, `? nick x`, `? hostmask user@host`,
///   `? casemap ascii|rfc1459|rfc1459-strict`, `? network name`.
/// - Buffer queries: `? buffers = console #a alice` (sidebar order),
///   `? members #c = ~q @op +voice plain` (`(away)` after an away member),
///   `? joined #c`, `? parted #c`, `? rejoin #c no|on-connect|<duration>`
///   (a parted channel; the duration is the wait before a kicked channel is
///   joined again), `? topic #c text|none`, `? modes #c +nt`,
///   `? line <buffer> text` (the last line's plain text; a final `*`
///   matches a prefix; `line-before` is the one before it), `? lines <buffer> n`, `? unread <buffer> n
///   highlights m`, `? focus <buffer>`, `? expanded yes|no`.
/// - Without a `!` or `@` before the first `>` or `<`, the script starts
///   already connected: the connect, its notice and dial, and `opened` run
///   unchecked.
/// - After every step, a view built only from the changes must match the
///   workspace: buffers, member lists and lines. Any change to a buffer's
///   counters, name, joined state, topic or modes, to the focus, to a
///   server's expansion or to the link's variant must come with its
///   `Change`.
/// - `#` at the start of a line is a comment; blank lines are skipped.
pub fn run_transcript(source: &str) -> Result<(), String> {
    run_transcript_hiding(source, |_| false)
}

/// Runs a transcript with the changes `hide` matches withheld from the
/// view.
pub fn run_transcript_hiding(source: &str, hide: fn(&Change) -> bool) -> Result<(), String> {
    let mut config = ServerConfig {
        id: ServerId(1),
        addresses: vec![address("irc.test:6667")],
        tls: Tls::Off,
        auto_reconnect: true,
        setup: setup("textual"),
        name: "test".to_owned(),
        autojoin: Vec::new(),
        wanted_caps: WANTED.iter().map(|c| (*c).to_owned()).collect(),
    };
    let mut driver: Option<Driver> = None;
    let mut errors = String::new();
    let mut ends_in_state = false;

    for (index, raw) in source.lines().enumerate() {
        let n = index + 1;
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (kind, rest) = line.split_at(1);
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        if kind == "=" {
            if driver.is_some() {
                return Err(format!("{n}: configuration after the first exchange"));
            }
            configure(&mut config, rest).map_err(|e| format!("{n}: {e}"))?;
            continue;
        }

        let t = driver.get_or_insert_with(|| {
            let mut t = Driver::new(config.clone(), hide);
            if !matches!(kind, "!" | "@") {
                t.net(network::Input::Connect);
                t.transport(t.latest.expect("dialed"), TransportEvent::Opened);
                t.sent.retain(|s| !s.starts_with("dial "));
                t.notices.retain(|n| !n.starts_with("Connecting to "));
            }
            t
        });
        ends_in_state = kind == "?" && is_end_state(rest);
        if matches!(kind, "<" | "!" | "@") {
            while let Some(extra) = t.sent.pop_front() {
                writeln!(errors, "{n}: unexpected sent {extra:?}").unwrap();
            }
        }
        let result = match kind {
            ">" => {
                match t.sent.pop_front() {
                    Some(got) if got == rest => {}
                    Some(got) => {
                        writeln!(errors, "{n}: expected sent {rest:?}, got {got:?}").unwrap();
                    }
                    None => {
                        writeln!(errors, "{n}: expected sent {rest:?}, nothing was sent").unwrap();
                    }
                }
                Ok(())
            }
            "<" => match t.latest {
                Some(conn) => parse_line(rest).map(|m| t.transport(conn, TransportEvent::Line(m))),
                None => Err("a server line before any dial".to_owned()),
            },
            "!" => event(t, rest),
            "@" => time(t, rest),
            "?" => {
                if let Some(got) = check_query(t, rest) {
                    writeln!(errors, "{n}: expected {rest:?}, got {got}").unwrap();
                }
                Ok(())
            }
            _ => Err(format!("unknown line kind {kind:?}")),
        };
        result.map_err(|e| format!("{n}: {e}"))?;
        for drift in t.drift.drain(..) {
            writeln!(errors, "{n}: view drift: {drift}").unwrap();
        }
    }

    if let Some(t) = &mut driver {
        while let Some(extra) = t.sent.pop_front() {
            writeln!(errors, "end: unexpected sent {extra:?}").unwrap();
        }
        while let Some(extra) = t.notices.pop_front() {
            writeln!(errors, "end: unchecked notice {extra:?}").unwrap();
        }
    }
    if !ends_in_state {
        errors.push_str("end: no end state (a link query such as `? registered`)\n");
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn configure(config: &mut ServerConfig, rest: &str) -> Result<(), String> {
    let (key, value) = rest.split_once(' ').unwrap_or((rest, ""));
    let s = &mut config.setup;
    match key {
        "nick" => value.clone_into(&mut s.nick),
        "alternates" => s.alternates = value.split_whitespace().map(str::to_owned).collect(),
        "user" => value.clone_into(&mut s.username),
        "realname" => value.clone_into(&mut s.realname),
        "auth" => {
            s.auth = match value {
                "none" => Auth::None,
                "sasl-plain" => Auth::SaslPlain,
                "nickserv" => Auth::NickServ,
                _ => return Err(format!("unknown auth {value:?}")),
            };
        }
        "server-password" => s.server_password = Some(Secret::new(value)),
        "account-password" => s.account_password = Some(Secret::new(value)),
        "caps" => config.wanted_caps = value.split_whitespace().map(str::to_owned).collect(),
        "id" => config.id = ServerId(value.parse().map_err(|_| format!("bad id {value:?}"))?),
        "addresses" => config.addresses = value.split_whitespace().map(address).collect(),
        "tls" => {
            config.tls = match value {
                "off" => Tls::Off,
                "on" => Tls::On { verify: true },
                "unverified" => Tls::On { verify: false },
                _ => return Err(format!("unknown tls {value:?}")),
            };
        }
        "auto-reconnect" => config.auto_reconnect = value == "on",
        "name" => value.clone_into(&mut config.name),
        "autojoin" => {
            config.autojoin = value
                .split_whitespace()
                .map(|entry| {
                    let (name, key) = entry.split_once(':').unwrap_or((entry, ""));
                    network::ChannelConfig {
                        name: name.to_owned(),
                        key: (!key.is_empty()).then(|| key.to_owned()),
                    }
                })
                .collect();
        }
        _ => return Err(format!("unknown setting {key:?}")),
    }
    Ok(())
}

fn address(text: &str) -> Address {
    let (host, port) = text.rsplit_once(':').expect("host:port");
    Address {
        host: host.to_owned(),
        port: port.parse().expect("a port number"),
    }
}

fn parse_line(raw: &str) -> Result<Message, String> {
    Message::parse(raw).map_err(|e| format!("bad line {raw:?}: {e}"))
}

/// Connects and registers with nothing checked, acknowledging the caps
/// the client requests among `offered`.
fn welcome(t: &mut Driver, offered: &str) -> Result<(), String> {
    t.net(network::Input::Connect);
    let conn = t.latest.ok_or("no dial")?;
    t.transport(conn, TransportEvent::Opened);
    let offered = if offered.is_empty() {
        "x-none"
    } else {
        offered
    };
    let ls = parse_line(&format!(":irc.test CAP * LS :{offered}"))?;
    t.transport(conn, TransportEvent::Line(ls));
    if let Some(req) = t.sent.iter().find_map(|s| s.strip_prefix("CAP REQ ")) {
        let ack = format!(":irc.test CAP * ACK {req}");
        t.transport(conn, TransportEvent::Line(parse_line(&ack)?));
    }
    let nick = t.network().config().setup.nick.clone();
    let welcome = parse_line(&format!(":irc.test 001 {nick} :Welcome"))?;
    t.transport(conn, TransportEvent::Line(welcome));
    t.sent.clear();
    t.notices.clear();
    if !matches!(t.network().link(), Link::Online { .. }) {
        return Err(format!("not registered: {:?}", t.network().link()));
    }
    Ok(())
}

fn event(t: &mut Driver, rest: &str) -> Result<(), String> {
    let (conn, rest) = match rest.strip_prefix('#') {
        Some(tail) => {
            let (n, tail) = tail.split_once(' ').ok_or("an event after the conn")?;
            let n = n.parse().map_err(|_| format!("bad conn {n:?}"))?;
            (Some(ConnId::new(n)), tail)
        }
        None => (t.latest, rest),
    };
    let (what, arg) = rest.split_once(' ').unwrap_or((rest, ""));
    let ui = |t: &mut Driver, action| t.input(Input::Ui(action));
    let event = match what {
        "connect" => {
            t.net(network::Input::Connect);
            return Ok(());
        }
        "disconnect" => {
            let reason = (!arg.is_empty()).then(|| arg.to_owned());
            t.net(network::Input::Disconnect { reason });
            return Ok(());
        }
        "welcome" => return welcome(t, arg),
        "focus" => {
            let id = t.buffer(arg)?.id();
            ui(t, UiAction::Select(id));
            return Ok(());
        }
        "close" => {
            let id = t.buffer(arg)?.id();
            ui(t, UiAction::Close(id));
            return Ok(());
        }
        "say" => {
            let (name, text) = arg.split_once(' ').ok_or("say <buffer> text")?;
            let buffer = t.buffer(name)?.id();
            t.input(Input::Say {
                buffer,
                text: text.to_owned(),
            });
            return Ok(());
        }
        "collapse" | "expand" => {
            let server = t.server;
            ui(
                t,
                UiAction::SetExpanded {
                    server,
                    expanded: what == "expand",
                },
            );
            return Ok(());
        }
        "window" => {
            let active = match arg {
                "active" => true,
                "inactive" => false,
                _ => return Err(format!("window active|inactive, not {arg:?}")),
            };
            ui(t, UiAction::WindowActive(active));
            return Ok(());
        }
        "opened" => TransportEvent::Opened,
        "line" => TransportEvent::Line(parse_line(arg)?),
        "closed" => TransportEvent::Closed(close_reason(arg)?),
        _ => return Err(format!("unknown event {what:?}")),
    };
    let conn = conn.ok_or("a transport event before any dial")?;
    t.transport(conn, event);
    Ok(())
}

fn close_reason(arg: &str) -> Result<CloseReason, String> {
    let (what, text) = arg.split_once(' ').unwrap_or((arg, ""));
    Ok(match what {
        "eof" => CloseReason::Eof,
        "timeout" => CloseReason::ConnectTimeout,
        "error" => CloseReason::Error(text.to_owned()),
        "bad-certificate" => {
            let bytes: Vec<u8> = (0..text.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(text.get(i..i + 2).unwrap_or("zz"), 16))
                .collect::<Result<_, _>>()
                .map_err(|_| format!("bad fingerprint {text:?}"))?;
            let sha256 = bytes.try_into().map_err(|_| "a fingerprint is 32 bytes")?;
            CloseReason::BadCertificate { sha256 }
        }
        _ => return Err(format!("unknown close reason {what:?}")),
    })
}

fn time(t: &mut Driver, rest: &str) -> Result<(), String> {
    if rest == "wake" {
        let at =
            t.ws.next_wake()
                .ok_or("`@ wake` with nothing to wake for")?;
        let by = at.saturating_duration_since(t.now);
        t.advance(by);
        return Ok(());
    }
    let by = rest
        .strip_prefix('+')
        .and_then(duration)
        .ok_or_else(|| format!("bad time {rest:?}, expected `+5s` or `wake`"))?;
    t.advance(by);
    Ok(())
}

/// `5s`, `4.613s` or `250ms`, to the millisecond.
fn duration(text: &str) -> Option<Duration> {
    if let Some(ms) = text.strip_suffix("ms") {
        return ms.parse().ok().map(Duration::from_millis);
    }
    let secs = text.strip_suffix('s')?;
    let (whole, frac) = secs.split_once('.').unwrap_or((secs, ""));
    if frac.len() > 3 {
        return None;
    }
    let whole: u64 = whole.parse().ok()?;
    let frac: u64 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<3}").parse().ok()?
    };
    Some(Duration::from_millis(whole * 1000 + frac))
}

fn show(d: Duration) -> String {
    let ms = d.as_millis();
    if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{}.{:03}s", ms / 1000, ms % 1000)
            .trim_end_matches('0')
            .to_owned()
    }
}

fn is_end_state(query: &str) -> bool {
    let what = query.split(' ').next().unwrap_or("");
    matches!(
        what,
        "registered" | "failed" | "registering" | "connecting" | "quitting" | "waiting" | "offline"
    )
}

fn ended_tag(why: &Ended) -> &'static str {
    match why {
        Ended::UserQuit => "user-quit",
        Ended::Closed(CloseReason::BadCertificate { .. }) => "bad-certificate",
        Ended::Closed(_) => "closed",
        Ended::ServerError(_) => "server-error",
        Ended::Banned(_) => "banned",
        Ended::RegistrationTimedOut => "registration-timed-out",
        Ended::PingTimeout => "ping-timeout",
        Ended::NoUsableNick { .. } => "no-usable-nick",
        Ended::BadSetup(_) => "bad-setup",
    }
}

fn last_ended(link: &Link) -> Option<&Ended> {
    match link {
        Link::Offline { last } => last.as_ref(),
        Link::Waiting { last, .. } => Some(last),
        _ => None,
    }
}

fn show_rows(rows: &[MemberRow]) -> String {
    rows.iter()
        .map(|r| {
            let symbol = r.prefix.map(|(_, s)| s.to_string()).unwrap_or_default();
            let away = if r.away { "(away)" } else { "" };
            format!("{symbol}{}{away}", r.nick)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Buffer queries: `None` when the query holds, else what was found.
fn check_buffer_query(t: &Driver, what: &str, arg: &str) -> Option<String> {
    let result: Result<Option<String>, String> = (|| {
        Ok(match what {
            "buffers" => {
                let want = arg.strip_prefix("= ").unwrap_or(arg);
                let got: Vec<String> = t
                    .network()
                    .buffers()
                    .iter()
                    .map(|b| t.label(b.id()))
                    .collect();
                let got = got.join(" ");
                (got != want).then_some(got)
            }
            "members" => {
                let (name, want) = arg.split_once(" =").ok_or("members #c = ...")?;
                let b = t.buffer(name)?;
                let got = show_rows(&rows(&t.ws, b.id()));
                (got != want.trim()).then_some(got)
            }
            "joined" | "parted" => {
                let b = t.buffer(arg)?;
                let joined = b.joined().is_some();
                (joined != (what == "joined"))
                    .then(|| if joined { "joined" } else { "not joined" }.to_owned())
            }
            "rejoin" => {
                let (name, want) = arg.split_once(' ').unwrap_or((arg, ""));
                let got = match t.buffer(name)?.rejoin() {
                    None => "not parted".to_owned(),
                    Some(Rejoin::No) => "no".to_owned(),
                    Some(Rejoin::OnConnect) => "on-connect".to_owned(),
                    Some(Rejoin::At(at)) => show(at.saturating_duration_since(t.now)),
                };
                (got != want).then_some(got)
            }
            "topic" => {
                let (name, want) = arg.split_once(' ').unwrap_or((arg, ""));
                let b = t.buffer(name)?;
                let got = b.joined().and_then(|j| j.topic()).unwrap_or("none");
                (got != want).then(|| got.to_owned())
            }
            "modes" => {
                let (name, want) = arg.split_once(' ').unwrap_or((arg, ""));
                let b = t.buffer(name)?;
                let got = b.joined().map(|j| j.modes()).unwrap_or_default();
                (got != want).then_some(got)
            }
            "line" | "line-before" => {
                let (name, want) = arg.split_once(' ').unwrap_or((arg, ""));
                let b = t.buffer(name)?;
                let back = usize::from(what == "line-before");
                let got = b
                    .scrollback()
                    .lines()
                    .rev()
                    .nth(back)
                    .map(|l| l.plain_text());
                let holds = got
                    .as_deref()
                    .is_some_and(|g| match want.strip_suffix('*') {
                        Some(prefix) => g.starts_with(prefix),
                        None => g == want,
                    });
                (!holds).then(|| format!("{got:?}"))
            }
            "lines" => {
                let (name, want) = arg.split_once(' ').unwrap_or((arg, ""));
                let b = t.buffer(name)?;
                let got = b.scrollback().len().to_string();
                (got != want).then_some(got)
            }
            "unread" => {
                let (name, want) = arg.split_once(' ').unwrap_or((arg, ""));
                let c = t.buffer(name)?.counters();
                let got = format!("{} highlights {}", c.unread, c.highlights);
                (got != want).then_some(got)
            }
            "focus" => {
                let got = t.ws.focus().map(|id| t.label(id));
                let want = t.buffer(arg)?.id();
                (t.ws.focus() != Some(want)).then(|| format!("{got:?}"))
            }
            "expanded" => {
                let got = if t.ws.is_expanded(t.server) {
                    "yes"
                } else {
                    "no"
                };
                (got != arg).then(|| got.to_owned())
            }
            _ => return Err(format!("unknown query {what:?}")),
        })
    })();
    match result {
        Ok(got) => got,
        Err(e) => Some(e),
    }
}

/// `None` when the query holds, else what was found instead.
fn check_query(t: &mut Driver, query: &str) -> Option<String> {
    let (what, arg) = query.split_once(' ').unwrap_or((query, ""));
    let net = t.network();
    let link = net.link();
    let session = net.session();
    let holds = match what {
        "notice" => {
            return match t.notices.pop_front() {
                Some(notice) if notice == arg => None,
                Some(notice) => Some(format!("notice {notice:?}")),
                None => Some("no notice".to_owned()),
            };
        }
        "registered" => {
            let mut words = arg.split(' ');
            let nick = words.next().unwrap_or("");
            let account = words.next();
            matches!(link, Link::Online { .. })
                && session.is_some_and(|s| s.nick() == nick && s.account() == account)
        }
        "registering" => matches!(link, Link::Registering { .. }),
        "connecting" => matches!(link, Link::Connecting { .. }),
        "quitting" => matches!(link, Link::Quitting { .. }),
        "waiting" => match (link, duration(arg)) {
            (Link::Waiting { until, .. }, Some(d)) => *until == t.now + d,
            _ => false,
        },
        "offline" => match link {
            Link::Offline { last: None } => arg.is_empty(),
            Link::Offline { last: Some(why) } => arg.is_empty() || ended_tag(why) == arg,
            _ => false,
        },
        "failed" => match (arg.split_once(' ').unwrap_or((arg, "")), last_ended(link)) {
            (("no-usable-nick", _), Some(Ended::NoUsableNick { .. })) => true,
            (("error", text), Some(Ended::ServerError(got))) => got == text,
            _ => false,
        },
        "next-wake" => {
            let got = net.next_wake().map(|at| at - t.now);
            let want = if arg == "none" { None } else { duration(arg) };
            if got == want {
                return None;
            }
            return Some(got.map_or("none".to_owned(), show));
        }
        "failures" => net.failures().to_string() == arg,
        "desired" => format!("{:?}", net.desired()).eq_ignore_ascii_case(arg),
        "nick" => session.is_some_and(|s| s.nick() == arg),
        "hostmask" => session.is_some_and(|s| s.hostmask() == Some(arg)),
        "casemap" => session.is_some_and(|s| Some(s.casemap()) == CaseMapping::from_token(arg)),
        "network" => session.is_some_and(|s| s.isupport().network.as_deref() == Some(arg)),
        _ => return check_buffer_query(t, what, arg),
    };
    if holds {
        return None;
    }
    Some(match link {
        Link::Waiting { until, last } => {
            format!("waiting {} after {last:?}", show(*until - t.now))
        }
        Link::Online { session, .. } => format!(
            "online as {} ({:?}, {:?}), failures {}",
            session.nick(),
            session.hostmask(),
            session.isupport().casemapping,
            net.failures()
        ),
        other => format!("{other:?}, failures {}", net.failures()),
    })
}
