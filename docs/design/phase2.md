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
let _tokio = runtime.enter();                                // held for the app's lifetime: the loop
                                                             // sleeps with tokio::time::sleep_until
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
Network { config, desired: Desired::{Online, Offline}, link: Link, failures: u32, buffers, ... }
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
- The loop waits on `rx.recv()` raced against `tokio::time::sleep_until(workspace.next_wake())`, with the runtime entered on the GTK thread (without it the sleep panics inside the glib future and the loop dies silently; measured). Not `glib::timeout_future`: it truncates to whole milliseconds, wakes just before the deadline, and the loop spins until the clock passes it (49 empty passes for one timer in a prototype).
- Timers are derived: `Network::next_wake()` is the minimum of the variant's deadline (retry due, 240 s watchdog, 270 s idle PING, 360 s idle close, quit grace) and a `later` queue (kick rejoin, NickServ wait). The loop sleeps until `Workspace::next_wake()`. Nothing is scheduled or cancelled.
- Backoff: 5 s doubling to a 300 s cap with jitter derived from (server id, failures), so tests are deterministic. `failures` lives on `Network`, not in `Link`, because it has to survive the live variants: it goes up when a link ends and resets only once a link has stayed `Online` for 60 s, so a server that accepts and drops us still backs off. No retry after a user quit, a bad certificate, a ban, ERROR "Excess Flood"/"SendQ", or `auto_reconnect = false`. Addresses rotate round-robin. `gio::NetworkMonitor` regaining the network skips a pending wait.

### Registration (candidate 1's protocol, candidate 2's step functions)

- `Caps::on_cap` is a pure step function, also used for `CAP NEW`/`DEL` after registration. It collects every `LS *` chunk, then packs `CAP REQ` lines to the byte budget. A NAK of a batch re-requests its caps one by one.
- `SaslState` step function: PLAIN (authzid = authcid = `username`, Textual's rule), EXTERNAL later. A 908 with another usable mechanism gets one retry. Failure continues with `CAP END` and prints why.
- `Auth` is explicit in config (`None`, `SaslPlain`, `NickServ`, later `SaslExternal`) instead of Textual's inference from a NickServ password.
- 433/432/437 before 001: alternate nicks, then `_` padding, then `NoUsableNick`. NICKLEN is not known yet (005 follows 001; measured on Ergo), so padding stops at 9 characters, the RFC 1459 default, or at the longest nick the server has already accepted. With `CAP LS 302` pending, servers hold the 433 until `CAP END`, so the ladder runs after the SASL step.
- After 001: on-connect commands, NickServ IDENTIFY for `Auth::NickServ`, then autojoin: configured autojoin channels plus the channels joined when the link dropped. Keyed and unkeyed JOINs on separate lines, under the byte budget.

### Identity (candidate 2's types)

`CaseMapping::nick(&str) -> NickKey` and `CaseMapping::chan(&str) -> ChanKey` are the only constructors, so maps keyed by nick or channel cannot be probed with a raw string or with the other kind. Default rfc1459 until 005; `Session::rekey` rebuilds the maps if 005 changes it. `Line::from_message` takes the casemap instead of comparing with `eq_ignore_ascii_case`.

### Members (candidate 1)

`Members { modes: HashMap<NickKey, ModeBits>, order: Vec<..> }`, sorted by (highest PREFIX index, folded nick). `ModeBits` is a bitset over the ISUPPORT PREFIX index, which removes Textual's "mode string must stay rank-sorted" invariant and its hard-coded `qOao`. Every mutation returns a positional `MemberDiff` (`Insert`, `Remove`, `Update`, `Move`, `Reset`) that the view splices into a `gio::ListStore`. GTK does no sorting. NAMES collects 353 lines and swaps the list at 366 (fixes the early return and the "ignore NAMES after the first" bug). After 366, a hidden `WHO #chan` (WHOX when offered) for channels under 500 members when `away-notify` is absent; its replies are not printed. Self JOIN on a joined channel is a no-op. No shared user table; QUIT and NICK walk the channels.

### Print funnel (candidate 3)

One function resolves a destination. It first strips STATUSMSG symbols with `Isupport::split_statusmsg` (an echoed `PRIVMSG @#chan` belongs to `#chan`, not to a query named `@#chan`). Then: a joined channel, a query created on demand for an incoming PM (our own echoed PRIVMSG goes to the target's query), or the console for server notices and numerics. A public message for a channel we are not in is dropped. QUIT and NICK print into every buffer the nick is in, computed before the state change. Highlight is decided here (own nick now, keyword lists in phase 3), the line goes into the buffer's `Scrollback` (`Rc<Line>`, model-owned, last N), counters bump (`nothing for the console; nothing if focused and the window is active`), and `Change::Line` then `Change::Counters` are emitted. With echo-message acked, own text is not printed locally. A line whose `msgid` tag equals one already in the buffer's scrollback is dropped, which covers the server delivering a message to our own nick twice (once as the echo, once as the delivery; measured on Ergo). `Line` takes its time and id from the caller (`Session` passes `now` and the next id from a per-workspace counter), so the core never reads the wall clock and two runs of one script produce equal scrollback.

### Changes and the view (candidate 3's pull model, candidate 1's GObject rows)

`Change` carries ids, plus the payloads that cannot be pulled afterwards (`Line`, `MemberDiff`). The view pulls everything else from `&Workspace`. Because the view applies a whole pass against the final workspace, a buffer that is rebuilt from state in a pass (its member list reset on `Change::Focus`, or its chat view created and filled from `Scrollback`) ignores that pass's `Line` and `MemberDiff` payloads for it: the final state already contains them. Applying both duplicated members and lines in a model of these rules. Widget commands (set input text, ask for confirmation) are `Effect::Ui`, not `Change`, so `Change` stays model facts.

- Sidebar: `gtk::ListView` over `gtk::TreeListModel`; root `gio::ListStore` of `ServerRow`, each with a child `ListStore` of `BufferRow`. Rows are small GObject subclasses with properties (label, kind, state, unread, highlights) updated in place, never replaced, so expansion and selection survive. The view keeps `HashMap<BufferId, BufferRow>` for lookup.
- Selection and expansion are both owned by the model. The view wires `SingleSelection`'s `notify::selected-item`, not `selection-changed`: GTK moves the selection itself when the selected row disappears (a collapse, a removal) and then emits only the notify. Each `TreeListRow`'s `notify::expanded` sends `UiAction::SetExpanded(server, bool)`. Rules in the model: collapsing the server of the focused buffer focuses that server's console; focusing a buffer under a collapsed server expands it first. Model-initiated moves come back as `Change::Focus` / `Change::Expanded`; the view expands the parent row, then calls `set_selected` (selecting the selected row emits nothing, so the echo costs one no-op message). Measured in a prototype: without these rules a collapse left the sidebar on one server and the model on another.
- Member list: one `gio::ListStore<MemberRow>`; on focus the view resets it from `workspace.members(focused)`, then applies `MemberDiff`s for the focused buffer only.
- Chat views: one `ChatView` per buffer, created on first focus, filled from the buffer's `Scrollback`, kept in a `gtk::Stack`. The replay deque in `ChatView` is replaced by the model's `Scrollback`. One shared web process.
- Title: a pure function of link state, nick, network, and channel name/count/modes or query nick/host. Topic goes to the chat view's topic bar.
- CSS classes: one module, `ui/classes.rs`, with constants and `row_classes(&row) -> &[&str]`; nothing else writes a class string. Window `tx-window`; sidebar `tx-sidebar`, `tx-server-row`, `tx-buffer-row`, `kind-channel`/`kind-query`, `state-joined`/`state-parted`/`state-error`/`state-offline`, `has-unread`, `has-highlight`, `tx-badge`; `tx-member-list`, `tx-member-row`, `rank-<letter>`, `is-away`; `tx-input`.

### Input (all pure, in `crates/core/src/input`)

- One `COMMANDS` table feeds dispatch and completion. Phase-2 set: `join part msg query me notice nick quit topic mode kick invite whois away close connect disconnect reconnect raw quote`. Unknown commands go out raw, uppercased. Console lines are commands with or without `/`; elsewhere `//x` sends `/x`, a lone `/` is text.
- `split_for_wire(text, budget)`: budget = 512 − len(`:nick!user@host CMD target :`) − 2, with the real hostmask once known, else the maximum; ACTION subtracts 9 more; never inside a UTF-8 sequence or a colour code; prefers a space in the last 25 bytes; reopens active formatting at the start of the next chunk, and those reopening bytes count against that chunk's budget (Ergo rejects an over-budget line with 417 rather than truncating it, so the text would be lost). Property tests: every chunk fits the budget, and the chunks with their inserted reopening codes removed join back to the input.
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
- e2e: the probe dumps sidebar rows (with classes and badges), member rows, title and input as JSON, and accepts `UiAction`s from a file. Scenarios: two servers, badges on unfocused channels, member list follows JOIN/PART/MODE, collapsing the focused buffer's server, Tab completion, server-side kill and reconnect with backoff, restart restores servers, selection and expansion, TLS on Ergo's TLS port (self-signed, so the scenario sets `verify = false` and also checks that `verify = true` ends in `BadCertificate`), SASL PLAIN against an Ergo account.

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

## Interrogation

Two adversaries probed this design before implementation (`docs/design/phase2-candidates/` has the brief). Corrections folded in above, each reproduced by rerunning the adversary's probe: the attempt counter lives on `Network`; pre-001 nick padding cannot use NICKLEN; a rebuilt buffer ignores that pass's payloads; `Line` takes time and id from the caller. Folded in from the text alone (the probes against Ergo were throttled on rerun): the split budget counts reopening codes; STATUSMSG is stripped before routing and echoes are deduplicated by `msgid`. From the GTK and runtime prototypes, all reproduced: the model owns expansion and the collapse/focus rules, the view listens to `notify::selected-item`, and the loop holds `runtime.enter()` and sleeps with tokio. Confirmed by the same prototypes: oo7 works from the tokio runtime while the GLib loop runs (D-Bus Secret Service, about 30 ms for create, search, read and delete); `rustls-platform-verifier` validates real certificates on Ubuntu 24.04; `related_view` exists in webkit6 0.6 for the shared web process; `BoxedAnyObject`-free GObject rows keep selection and expansion across in-place updates and sibling inserts.

## Implementation reconciliation

Record accepted deviations here, with their source, before the next unit starts.

Unit 3 (registration):

- Registration takes no clock. The 240 s watchdog is `Link::Registering`'s deadline (unit 4), so the transcript runner rejects `@ +Ns` until a script drives a `Link`.
- `Registration::step` consumes the registration and returns `Progress::{Registering, Registered, Failed}`, so the secrets it holds are dropped at 001 or failure by construction.
- `Registration::start` takes the wanted capability list (`caps::WANTED` in the app) and adds `sasl` itself for `Auth::SaslPlain`. Tests pass a longer list to exercise REQ packing past 512 bytes.
- Padding: "the longest nick the server has already accepted" is read as the longest nick that drew 433 or 437 (taken, so its length was valid). A configured nick already at the limit gets one try with its last character replaced by `_`; without it a 432 for a too-long nick could never register.
- 908: `Auth::SaslPlain` has one mechanism, so phase 2 never retries; `sasl-908.irc` shows that path and the retry is unit-tested in `irc::sasl` with EXTERNAL then PLAIN. Measured on Ergo: an unknown mechanism draws 904 with no 908.
- CAP answers (review of fd1eda1): `Caps` tracks each outstanding cap with the REQ it went out in, not whole batches. An ACK or NAK settles every REQ it names a cap of, so a partial or superset ACK, a NAK of a subset, or a DEL of a pending cap cannot stall registration. Only requested caps are enabled. A NAK re-requests only caps from a refused multi-cap REQ, one by one, and REQ lines are built only from the wanted list, never from server text. A CAP line with no list after the subcommand (or after `*`) is ignored. With `Auth::SaslPlain`, a 001 that arrives before CAP settled (a server without CAP) prints "the server does not offer SASL".
- Known limitation: the nick ladder counts nick length in chars, while servers count NICKLEN in bytes, so a multi-byte nick can be padded past the server's limit and draw 432.
- `USER name 0 * Textual`: `to_wire` adds the trailing colon only when needed, so a one-word real name goes out without it. Valid, Ergo accepts it, and phase 1 already sent it this way; the usage example above shows the colon.

Unit 4 (lifecycle and transport):

- `Link::Registering { conn, reg, deadline }` holds no `Session`. Nothing a `Session` holds is known before 001, so it is built from `Registered` at 001 instead of being half-filled at dial time.
- `Waiting { until, last }` and `Connecting { conn, address }` carry no `attempt`. `Network` holds the address cursor (round-robin) next to `failures`, and `failures` is the attempt count. `Online` adds `since` and `idle: Idle { last_heard, pinged }`, from which the 270 s PING and the 360 s close are derived.
- The 60 s reset is applied when a link ends: a link that was `Online` for at least 60 s sets `failures` to 0 before this failure counts. The backoff is the same as resetting at the 60 s mark, with no extra wake. An explicit connect from `Offline` also resets `failures`; one during `Waiting` dials at once and keeps the count.
- Jitter only shortens: each wait is the 5 s doubling base (capped at 300 s) less up to a tenth, from a hash of (server id, failures). The cap holds and the first retry comes within 4.5 to 5 s.
- No retry after a user quit, `Closed(CloseReason::BadCertificate { sha256 })` (nested in `Ended::Closed` rather than a top-level variant), `465` at any time (`Ended::Banned`), an ERROR whose text contains `Excess Flood` or `SendQ` (Textual also requires the `Closing Link:` prefix and the exact parenthesised suffix), a bad `Setup`, or `auto_reconnect = false`. Everything else retries, including `NoUsableNick` (the nick may be free later) and a plain ERROR such as a kill.
- `Disconnect` while waiting cancels the retry without a notice; while dialing or registering it hangs up without QUIT; online it sends QUIT and waits 5 s in `Quitting`, which the server's ERROR, its close or the grace period ends. A `Connect` while quitting dials again once the quit is done (the shape of `/reconnect`).
- `ConnId` is unique within one `Network`. The workspace (unit 6) routes transport events by (server, conn).
- `ServerConfig` carries `wanted_caps` (`caps::WANTED` in the app), so transcripts can keep replacing the list with `= caps`.
- The transcript runner drives a `Network` for every script, so `@ +Ns` now works (the unit 3 restriction is gone). A script with no `!` or `@` before its first exchange starts connected. Two unit-3 transcripts gained the hangup and retry notice that ending a link now produces.
- The transport forwards PING to the model after answering it, so a server's PING counts as liveness; the model never answers it.
- `Connection::hangup` (or dropping the handle) aborts the task: no `Closed` follows and queued lines are not flushed. The model only hangs up after QUIT has been answered or its grace has passed, or when the link is dead anyway.
- Flood pacing is in: at most 6 queued lines per 2 s (Textual's defaults), as a sliding window; PONG is exempt. The 30 s connect timeout covers the TCP connect and the TLS handshake together.
- Transition: `src/irc/connection.rs`, `tests/connection.rs` and the phase-1 `main.rs` stay unchanged and keep `scripts/e2e.py` passing. `io/transport.rs` has its own copy of `LineReader`; the old one goes when unit 6 switches the app over and deletes `connection.rs`.

Unit 5 (membership, print funnel, scrollback, counters):

- Members live on the buffer, not in `Session`: `Body::Channel(ChannelState::{Parted, Joined(Joined { topic, modes, members, names, sync })})`. Buffers outlive links, and a parted channel has no field to hold members, a topic or modes. `Network::end` parts every channel (`MemberDiff::Reset` for each), so a disconnect still drops all membership in one place.
- `Network` owns its `Buffers` (console, channels, queries). `Network::new` and `update` are crate-private: `Workspace` is the only way in. It routes `Input::Network { server, input }` by `ServerId` and tags each effect with its server.
- `BufferId(u64)` and `LineId(u64)` come from the workspace's `Ids`, two counters (`next_buffer`, `next_line`) that hand out ids in order, so a script gives the same ids every run. Known follow-up: a `u64` cannot hold a config UUID, so unit 8 must widen `BufferId` or map each configured channel's UUID to an id. `Now { mono, wall }` is passed to `Workspace::update`; lines take `wall` and the next id through a `Stamp`, and nothing in the core reads the clock.
- `Line::from_message(msg, me, &Isupport, stamp)` returns `Destination::{Channel, Query, Nick, Server}`; `Nick` (QUIT, NICK) resolves to the joined channels the nick is in plus its query, before the state changes. Lines carry `msgid`.
- The workspace applies the counter rule after the network's step, because it owns focus and window state; `Change::Line` is still followed directly by its `Change::Counters`. Following Textual (`IRCClient.m` print completion blocks), only PRIVMSG and ACTION from someone else count in a channel, and PRIVMSG, ACTION and NOTICE in a query; events and our own echoes do not. A channel NOTICE only notifies in Textual (`_receiveText_Public`), so it neither counts nor highlights; a NOTICE in a query counts as unread (`IRCClient.m:6602`) but never as a highlight. Focusing a buffer, or activating the window on it, clears its counters.
- `Effect::Notice` is gone: connection notices print into the console as client lines (command `-100`, Textual's default). Registration chatter and unknown numerics print there too, except CAP, AUTHENTICATE, 432/433/437 and 900-908, which registration explains with its own notices. 401-596 (not 422) print as Textual's `Message(n): ...`. 353 and 366 never print (user-requested `/names` is unit 7); 329 is ignored; 331 clears the topic and prints to the console as an unknown numeric, as Textual does.
- Routing details: only PRIVMSG opens a query; a user's NOTICE goes to its open query, else the console. A self JOIN sends `MODE #chan` (Textual's `requestModesForChannel`), so 324 fills the modes. A NICK renames the query unless a query with the new nick is open.
- The hidden WHO goes out only after a join's first 366, not after a later NAMES. WHOX is `WHO #chan %tcuhnfa,152`; the token tells our 354 replies apart. WHO replies set away, user, host and account, not modes (NAMES has already given them).
- Highlight follows Textual's default "exact" setting through `TVCLogRenderer matchKeywordsUsingNormalMatching` and `sectionOfBodyIsSurroundedByNonAlphabeticals`: the left neighbour is checked only when the match starts with a letter or digit, and the right neighbour only when it ends with one, so `foo_` highlights in `foo_bar` and `x[y]` in `x[y]z`, but `x[y]` does not in `ax[y]`. It matches the body with formatting stripped (`style::body::strip`), folds both sides by the server's casemap, and skips a match that overlaps a link. Until links are rendered, `style::body::links` stands in for `TLOLinkParser`: a token's `scheme://` (letters before `://`) or a `www.` not preceded by a letter or digit, up to the next whitespace. Only PRIVMSG and ACTION are scanned (`scanForKeywords`). Keyword lists are phase 3.
- Scrollback keeps 1000 lines per buffer (Textual's visible limit defaults to unlimited).
- A CASEMAPPING change re-keys every member list (a `Reset` each). Known limitation: a PREFIX change after members exist does not re-map their mode bits.
- Added for testability: `workspace::Input::Say { buffer, text }` sends one PRIVMSG and prints it locally unless echo-message is on (unit 7 replaces it with commands and splitting), and `UiAction::Close` closes a query or channel (PART if joined) and moves the focus to the row above.
- Not in this unit: KICK auto-rejoin, rejoin after reconnect and autojoin (registration's "After 001" steps).
- Hostile input (review of a0a2f4f): a NAMES token that leaves an empty nick (only prefix symbols, or starting with `!` or `@` after them) adds no member; a PRIVMSG or NOTICE whose query nick would be empty goes nowhere; an empty or valueless `msgid` is no msgid. Selecting a collapsed server's console leaves it collapsed, since the console is the server row.
- Known limitations (review of a0a2f4f), not fixed: a hidden WHO that never gets its 315 leaves `Sync::Who` set and swallows the user's own later WHO replies for that channel; a 353 collected before a QUIT can bring the quitter back at 366; PRIVMSG and NOTICE from users before 001 are dropped; CTCP other than ACTION is dropped; `Workspace::add_network` asserts on a duplicate `ServerId`, so unit 8 must reject duplicates when it loads the config.
- The transcript runner now drives a `Workspace` with one network for every script, and after each step checks that a view built only from `Change`s (members by the design's reset rule, lines, buffer ids) matches the workspace. It also compares a snapshot of the workspace before and after each step: a change to a buffer's counters, name, joined state, topic or modes, to the focus or to a server's expansion without its `Counters`, `BufferChanged`, `Focus` or `Expanded` fails the scenario (`the_view_check_fails_when_a_change_is_missing` hides each kind to prove it). `! welcome [caps]` registers without checks for scripts about later behaviour.

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
- Unit 4 review follow-ups (accepted): a stable session (60 s online) resets `failures` when it is disconnected, because the link then ends from `Quitting` where `end` cannot see `since`; `Connecting` has a 45 s model deadline so a transport task that dies silently cannot strand a link; QUIT skips the paced queue like PONG. Known limitations, not fixed: a write error ends the transport before buffered input is read, so a server's final ERROR (e.g. Excess Flood) can be lost and the link retries; a peer that stops reading fills the socket and stalls reading and PONG until the model's idle close.
