# Phase 2 design: the client model

This is the contract for phase 2. It was synthesized from three candidate designs (`docs/design/phase2-candidates/`), scored against `rubric.md` there by me and by a cross-judge. Read the candidates for detail; this file says which parts we take and the decisions that bind the implementation.

## Problem

Phase 1 renders one hard-coded channel. Phase 2 needs many servers, a sidebar and member list, input with history and completion, TLS, SASL, secrets and a saved config. The one-way door is who owns protocol and UI state and how a change travels from a socket line to a widget and back. Constraints: the GTK main loop is single-threaded; sockets run on tokio; GTK emits signals synchronously while a model is being changed; Textual's bugs (no CASEMAPPING, broken CAP LS 302, fixed 20 s reconnect, NAMES early return) must not be copied; widgets need stable CSS class names for the later "Textual for Mac" stylesheet.

## Usage (caller's view)

```rust
// main.rs, activate
let boot = store::load()?;                                   // config.toml + state.toml, migrated
let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Msg>();
let workspace = Workspace::new(boot);
let view = View::build(app, &style, tx.clone());             // every signal handler only does tx.send(Msg::Ui(..))
let io = Io::new(runtime.handle().clone(), tx.clone(), boot.paths);
io.load_secrets();                                           // later Msg::SecretsLoaded
glib::spawn_future_local(app::run(workspace, view, io, rx));

// app::run, the only loop
loop {
    let msg = next_message_or_wake(&mut rx, workspace.next_wake()).await;
    let mut out = Out::default();
    workspace.update(msg, clock.now(), &mut out);            // drains up to 256 queued msgs per pass
    io.execute(out.effects);                                 // dial, send, hangup, save, secrets
    view.apply(&workspace, &out.changes);                    // reads &Workspace, patches widgets
}
```

```rust
// A protocol test: no GTK, no socket, no clock.
let mut t = Script::new(server("ergo").auth(Auth::SaslPlain));
t.connect();
t.opened();
t.sent(["CAP LS 302", "NICK textual", "USER textual 0 * :Textual"]);
t.recv(":ergo CAP * LS * :multi-prefix sasl=PLAIN");
t.sent_nothing();
t.recv(":ergo CAP * LS :server-time echo-message");
t.sent(["CAP REQ :echo-message multi-prefix sasl server-time"]);
t.advance(Duration::from_secs(241));          // virtual time: registration watchdog
t.expect_hangup();
```

## Shape

### Crates (from candidate 3)

A Cargo workspace. `crates/core` (`textual-core`) holds `irc/`, `model/`, `input/`, `config/` (schema and migration only), `line.rs` and `style/`. It does not depend on gtk, adw, glib, gio, webkit6 or tokio, so a reducer that touches GTK does not build. The root crate (`textual-gtk`) holds `app.rs`, `io/` (transport, secrets, store) and `ui/`.

### Ownership (from candidate 3, with candidate 1's types)

`Workspace` owns every piece of protocol and UI-visible state: sessions, buffers, members, topics, unread and highlight counters, focus, input memory, scrollback. It is a plain struct moved into the `app::run` future. There is no `Rc<RefCell>` around it and no lock. Widgets cannot reach it: GTK signal handlers only send `Msg::Ui(UiAction)`. A signal fired while the view applies a change therefore enqueues a message and cannot re-enter the model. Every `UiAction` is idempotent (`Select(already focused)` is a no-op), so the echo of a programmatic selection costs nothing.

The tokio side owns bytes: dial, rustls, framing (the existing `LineReader`), an ordered write queue, flood pacing, and **PONG** (from candidate 1: stateless, and it keeps the link alive while the main thread renders a NAMES burst). It holds no nick, caps or channels. It accepts only `WireLine` (a validated line), so a line that cannot be sent never reaches it and phase 1's `Event::Refused` goes away.

### Lifecycle (candidate 1's `Link`, candidate 2's `Desired`, candidate 3's derived timers)

```
Network { config, desired: Desired::{Online, Offline}, link: Link, buffers, ... }
Link = Offline { last: Option<Ended> }
     | Waiting { until, attempt, last }
     | Connecting { conn: ConnId, attempt, address }
     | Registering { conn, reg: Registration, session: Session }
     | Online { conn, session: Session, since }
     | Quitting { conn, session: Session, until }
```

- `Session` (nick, hostmask, acked caps, `Isupport`, casemap, channel states and members) exists only inside the live variants. A disconnect drops it, so no reset can forget a field. `Registration` (CAP phase, SASL phase, nick attempt, the one-shot secrets) exists only while registering, so secrets are dropped at 001.
- `Desired` is separate from `Link`: "offline because the user said so" and "offline and retrying" are different states.
- Every transport event carries `ConnId`; an event from another conn is dropped. `Hangup` of a stale conn is a no-op.
- Timers are derived: `Network::next_wake()` is the minimum of the variant's deadline (retry due, 240 s watchdog, 270 s idle PING, 360 s idle close, quit grace) and a `later` queue (kick rejoin, NickServ wait). The loop sleeps until `Workspace::next_wake()`. Nothing is scheduled or cancelled.
- Backoff: 5 s doubling to a 300 s cap with jitter derived from (server id, attempt), so tests are deterministic. The attempt counter resets after 60 s online. No retry after a user quit, a bad certificate, a ban, ERROR "Excess Flood"/"SendQ", or `auto_reconnect = false`. Addresses rotate round-robin. `gio::NetworkMonitor` regaining the network skips a pending wait.

### Registration (candidate 1's protocol, candidate 2's step functions)

- `Caps::on_cap` is a pure step function, also used for `CAP NEW`/`DEL` after registration. It collects every `LS *` chunk, then packs `CAP REQ` lines to the byte budget. A NAK of a batch re-requests its caps one by one.
- `SaslState` step function: PLAIN (authzid = authcid = `username`, Textual's rule), EXTERNAL later. A 908 with another usable mechanism gets one retry. Failure continues with `CAP END` and prints why.
- `Auth` is explicit in config (`None`, `SaslPlain`, `NickServ`, later `SaslExternal`) instead of Textual's inference from a NickServ password.
- 433/432/437 before 001: alternate nicks, then `_` padding to NICKLEN, then `NoUsableNick`.
- After 001: on-connect commands, NickServ IDENTIFY for `Auth::NickServ`, then autojoin: configured autojoin channels plus the channels joined when the link dropped. Keyed and unkeyed JOINs on separate lines, under the byte budget.

### Identity (candidate 2's types)

`CaseMapping::nick(&str) -> NickKey` and `CaseMapping::chan(&str) -> ChanKey` are the only constructors, so maps keyed by nick or channel cannot be probed with a raw string or with the other kind. Default rfc1459 until 005; `Session::rekey` rebuilds the maps if 005 changes it. `Line::from_message` takes the casemap instead of comparing with `eq_ignore_ascii_case`.

### Members (candidate 1)

`Members { modes: HashMap<NickKey, ModeBits>, order: Vec<..> }`, sorted by (highest PREFIX index, folded nick). `ModeBits` is a bitset over the ISUPPORT PREFIX index, which removes Textual's "mode string must stay rank-sorted" invariant and its hard-coded `qOao`. Every mutation returns a positional `MemberDiff` (`Insert`, `Remove`, `Update`, `Move`, `Reset`) that the view splices into a `gio::ListStore`. GTK does no sorting. NAMES collects 353 lines and swaps the list at 366 (fixes the early return and the "ignore NAMES after the first" bug). After 366, a hidden `WHO #chan` (WHOX when offered) for channels under 500 members when `away-notify` is absent; its replies are not printed. Self JOIN on a joined channel is a no-op. No shared user table; QUIT and NICK walk the channels.

### Print funnel (candidate 3)

One function resolves a destination: a joined channel, a query created on demand for an incoming PM (our own echoed PRIVMSG goes to the target's query), or the console for server notices and numerics. A public message for a channel we are not in is dropped. QUIT and NICK print into every buffer the nick is in, computed before the state change. Highlight is decided here (own nick now, keyword lists in phase 3), the line goes into the buffer's `Scrollback` (`Rc<Line>`, model-owned, last N), counters bump (`nothing for the console; nothing if focused and the window is active`), and `Change::Line` then `Change::Counters` are emitted. With echo-message acked, own text is not printed locally.

### Changes and the view (candidate 3's pull model, candidate 1's GObject rows)

`Change` carries ids, plus the payloads that cannot be pulled afterwards (`Line`, `MemberDiff`). The view pulls everything else from `&Workspace`. Widget commands (set input text, ask for confirmation) are `Effect::Ui`, not `Change`, so `Change` stays model facts.

- Sidebar: `gtk::ListView` over `gtk::TreeListModel`; root `gio::ListStore` of `ServerRow`, each with a child `ListStore` of `BufferRow`. Rows are small GObject subclasses with properties (label, kind, state, unread, highlights) updated in place, never replaced, so expansion and selection survive. The view keeps `HashMap<BufferId, BufferRow>` for lookup.
- Selection: `gtk::SingleSelection` sends `UiAction::Select`; model-initiated focus moves come back as `Change::Focus` and the view calls `set_selected` (selecting the selected row emits nothing).
- Member list: one `gio::ListStore<MemberRow>`; on focus the view resets it from `workspace.members(focused)`, then applies `MemberDiff`s for the focused buffer only.
- Chat views: one `ChatView` per buffer, created on first focus, filled from the buffer's `Scrollback`, kept in a `gtk::Stack`. The replay deque in `ChatView` is replaced by the model's `Scrollback`. One shared web process.
- Title: a pure function of link state, nick, network, and channel name/count/modes or query nick/host. Topic goes to the chat view's topic bar.
- CSS classes: one module, `ui/classes.rs`, with constants and `row_classes(&row) -> &[&str]`; nothing else writes a class string. Window `tx-window`; sidebar `tx-sidebar`, `tx-server-row`, `tx-buffer-row`, `kind-channel`/`kind-query`, `state-joined`/`state-parted`/`state-error`/`state-offline`, `has-unread`, `has-highlight`, `tx-badge`; `tx-member-list`, `tx-member-row`, `rank-<letter>`, `is-away`; `tx-input`.

### Input (all pure, in `crates/core/src/input`)

- One `COMMANDS` table feeds dispatch and completion. Phase-2 set: `join part msg query me notice nick quit topic mode kick invite whois away close connect disconnect reconnect raw quote`. Unknown commands go out raw, uppercased. Console lines are commands with or without `/`; elsewhere `//x` sends `/x`, a lone `/` is text.
- `split_for_wire(text, budget)`: budget from the real hostmask once known, else the maximum; ACTION subtracts 9; never inside a UTF-8 sequence or a colour code; prefers a space in the last 25 bytes; reopens active formatting on the next chunk. Property test: chunks join back to the input and each fits.
- More than 4 lines or 4×510 bytes asks first (`Effect::Ui(Confirm::Flood)`), then the view re-sends with `confirmed: true`.
- `History`: per buffer (global as an option), 100 entries, consecutive duplicates skipped, the draft held in a separate stash rather than pushed into history (a deliberate difference from Textual).
- `Completion`: Textual's algorithm as documented in `docs/research/phase2-window-and-input.md`, fed by `Session::completion_source(buffer)`.

### Config and secrets (candidates agree)

- TOML, two files: `$XDG_CONFIG_HOME/textual-gtk/config.toml` (servers, channels, identity, auth; written on edits) and `$XDG_STATE_HOME/textual-gtk/state.toml` (selection, expanded rows, window size, `last_server_time`; at most every 300 s and at quit). Defaults stripped (`serde(default, skip_serializing_if)`), `version` plus a migration chain, atomic write via temp file and rename. A file newer than the build is loaded read-only.
- Secrets: `oo7`, items `{application: textual-gtk, kind, owner: <uuid>}`. `Secret` has no `Debug` value, `Display` or `Serialize`. Loaded once at start on tokio; `Msg::SecretsLoaded` (success or failure) gates connecting, so a locked keyring never blocks the window.
- TLS: `tokio-rustls` with `rustls-platform-verifier`. `verify = false` is Textual's `validateServerCertificateChain = NO`. A rejected chain ends with `Ended::BadCertificate { sha256 }` and no retry; the trust prompt is phase 3.
- Config edits arrive as `UiAction::EditServer(config)`; a pure `live_changes(old, new)` applies channel adds/removes now and connection settings at next connect.
- Phase 2 includes a minimal Add Server dialog (name, address, port, TLS, nick, auth, password), because multi-server is unusable without it. Full server and channel properties are phase 3.

### Testing

- Unit: casemap vectors, `Isupport`, `Caps`, `SaslState`, `Members` ordering and diffs, `History`, `Completion`, `split_for_wire` property test, config round-trip and migrations.
- `Script` harness over `Workspace` with virtual time (`advance`), plus `tests/scenarios/*.irc` transcripts (`> sent`, `< received`, `@ +300s`, `? members #c = @a +b c`) run by one runner.
- Transport tests with real sockets: framing, PONG, idle close, flood pacing, rustls against an `rcgen` certificate.
- e2e: the probe dumps sidebar rows (with classes and badges), member rows, title and input as JSON, and accepts `UiAction`s from a file. Scenarios: two servers, badges on unfocused channels, member list follows JOIN/PART/MODE, Tab completion, server-side kill and reconnect with backoff, restart restores servers and selection, TLS on Ergo's TLS port, SASL PLAIN against an Ergo account.

## Synthesis decision

Base: candidate 3 (Sonnet), for its structure: the message-passing loop (re-entrancy is impossible rather than guarded), the `textual-core` crate (layering enforced by the build), model-owned counters and scrollback (testable headless), and derived timers. The cross-judge recommended candidate 1 (28 vs 25), mainly for protocol completeness and a sidebar that works as written; both are grafted in. Candidate 1 had seen the rubric while writing, which the judge found in its framing but not its decisions.

Grafted from candidate 1: `Link` with `Session`/`Registration` only inside live variants; `ModeBits` and positional `MemberDiff`; NAK one-by-one, 908 retry, hidden WHO after NAMES, rejoin of previously joined channels; explicit `Auth`; GObject rows updated in place (fixes candidate 3's sidebar collapse, the judge's main objection); PONG on the transport.

Grafted from candidate 2: `Desired` separate from `Link`; distinct `NickKey`/`ChanKey`; `Caps` as a step function that also serves `CAP NEW`/`DEL`.

Rejected: candidate 2's counters in GTK rows and `Rc<RefCell>` dispatch (split ownership, re-entrancy by discipline); candidate 3's raw-line field, plugin-shaped command registry, trust prompt and fingerprint pinning, view LRU (phase 3 or later); widget commands inside `Change`.

## Tradeoffs accepted

- We accept the reducer on the GTK thread in exchange for one owner and synchronous reads (completion, title, counters). PONG on tokio bounds the cost of a slow frame.
- We accept a message hop for every UI action in exchange for no re-entrancy and a scriptable UI.
- We accept a two-crate workspace and a one-time move in exchange for a build-enforced boundary.
- We accept counting unread before render (Textual counts after) because our highlight decision is pure and made first; the badge still updates after the line is appended.
- We accept no shared user table; away, host and account updates walk the channels.
- We accept the draft stash instead of Textual's draft-in-history behaviour.

## Alternatives considered

Per the candidates: protocol state in a tokio actor with a UI mirror (two copies of truth); a GObject-native model (untestable without GLib, two indexes); `Arc<Mutex<World>>` (locks, half-applied reads); state diffing (O(members) per line); GSettings for servers (nested lists, installed schema needed to run). All lost on interface depth or single ownership; see each candidate's section.

## Implementation reconciliation

Record accepted deviations here, with their source, before the next unit starts.

## Open questions (defaults applied; Michael can override)

- `/join` of an unconfigured channel adds it to `config.toml` with autojoin on (Textual's behaviour as far as the notes say; to check in `IRCClient joinChannel`).
- Rejoin after reconnect covers configured autojoin channels plus channels that were joined.
- A message typed while offline stays in the field with a "Not connected" notice.
- NickServ notice scraping (for `autojoin_waits_for_nickserv` without SASL) is phase 3; phase 2 waits for SASL success or 3 s.
- One shared web process for all chat views; a crash reloads every view from scrollback.

## Units, in order

Each unit ends green (`scripts/check.sh`, plus `scripts/e2e.py` from unit 6) and gets an independent review before it is pushed.

1. Workspace split: move `irc/message.rs`, `line.rs`, `style/` and their tests into `crates/core`; no behaviour change.
2. `casemap` (NickKey/ChanKey), `isupport`, `WireLine`.
3. `Caps`, `SaslState`, `Registration` and the `Script` harness; CAP 302, SASL and nick-ladder scripts.
4. `Network`/`Link`/`Session` lifecycle with backoff and derived timers; transport rewritten as `io/transport.rs` with TLS and PONG.
5. Membership (`Members`, NAMES/WHO, JOIN/PART/QUIT/KICK/NICK/MODE), print funnel, scrollback, counters.
6. View: sidebar, member list, chat-view stack, title, CSS classes; probe JSON; e2e for two servers and badges.
7. Input: commands, split, history, completion; e2e for completion and sending.
8. Config and secrets: TOML store, migrations, oo7, Add Server dialog; e2e restart restores state; SASL against Ergo.
