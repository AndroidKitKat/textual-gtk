# Candidate 3: a sans-IO core, one owner on the GTK thread, a message loop

Types are in `sketch.rs` (compiles with `cargo check`, bodies `todo!()`, std only). `check/src/usage.rs` compiles the loop and a `Sink` against those types.

## Problem

Phase 1 proved the render path with one hard-coded connection. Phase 2 needs many servers, a sidebar and member list, input with history and completion, TLS/SASL/secrets and persisted config. The one-way door is who owns protocol and UI state and how a change travels from a socket line to a widget.

Constraints Phase A surfaced:

- `irc/connection.rs` owns registration, PING, 433 retry and autojoin inside a tokio task. That is protocol state the UI, reconnect and config edits all need, trapped behind an event channel.
- Phase 1's `main.rs` already shows the failure mode: `own_nick` is an `Rc<RefCell>`, the destination filter is an `eq_ignore_ascii_case` in a closure, and `Line::from_message` guesses `ChannelsOf(nick)` because nobody tracks members.
- Textual's model calls `mainWindow()` globally from IRCClient. Its handlers run on main, so single-threaded ownership is what the original does, minus the coupling.
- Textual bugs not to copy: no CASEMAPPING; CAP LS 302 multiline; fixed 20 s reconnect; NAMES early return and "ignore NAMES after 366"; self-JOIN on an active channel returns early; a 5-minute GC timer for users; reconnect state cleared by accident.
- GTK signals fire synchronously while a model is being updated. A shared `Rc<RefCell<model>>` read from signal handlers panics the first time a programmatic selection change re-enters.
- The appearance decision needs stable CSS class names on every widget (listed below).

## Usage (caller's view)

Written first. The sketch is derived from these four call sites.

**1. `main.rs` after phase 2 (about 30 lines).**

```rust
app.connect_activate(move |app| {
    let boot = store::load().unwrap_or_default();            // servers.toml + state.toml, migrated
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Msg>();
    let ws   = Workspace::new(boot.servers, boot.state, boot.prefs);
    let view = View::build(app, &style, &pages, tx.clone());    // every signal handler does tx.send(Msg::Ui(..))
    let io   = Io::new(runtime.handle().clone(), tx.clone(), boot.dirs);
    io.load_secrets();                                          // Msg::SecretsLoaded when done
    glib::spawn_future_local(app::run(ws, vec![Box::new(view), Box::new(io.persist_sink())], io, rx));
});
```

**2. A session scenario test. This is the spec for the reducer and needs no sockets, no GTK, no clock.**

```rust
let mut t = Scenario::new(server_with_sasl("irc.example", 6697));
t.connect();                                  // SessionInput::Connect
t.expect_dial("irc.example", 6697, tls = true);
t.opened();                                   // NetEvent::Opened
t.expect_sent(&["CAP LS 302", "NICK tester", "USER tuser 0 * :Test"]);
t.server("CAP * LS * :multi-prefix away-notify");
t.server("CAP * LS :sasl=PLAIN server-time");
t.expect_sent(&["CAP REQ :multi-prefix away-notify sasl server-time"]);   // after the last chunk only
t.server("CAP * ACK :multi-prefix away-notify sasl server-time");
t.expect_sent(&["AUTHENTICATE PLAIN"]);
t.server("AUTHENTICATE +");
t.expect_sent(&["AUTHENTICATE dHVzZXIAdHVzZXIAcHc="]);
t.server(":s 903 tester :SASL authentication successful");
t.expect_sent(&["CAP END"]);
t.server(":s 001 tester :Welcome");
t.expect_sent(&["JOIN #rust,#gtk"]);
t.advance(Duration::from_secs(300));          // virtual time
t.expect_sent(&["PING irc.example"]);
t.advance(Duration::from_secs(100));
t.expect_phase(PhaseView::Reconnecting { attempt: 1 });
```

The same scenarios also live as text files (`tests/scenarios/*.irc`, a tiny DSL: `> sent line`, `< server line`, `@ +300s`, `? members #rust = @alice +bob carol`). One runner executes them all. A new protocol bug becomes a new file.

**3. A user types a line.** The widget does not know about IRC.

```rust
// ui/input_bar.rs, in the key handler
tx.send(Msg::Ui(UiAction::Submit { buffer: current, text: view.text(), confirmed: false }));
// model: Workspace::update → classify → Session::handle(Submit) → split_for_wire → WireLine::new
//        → Effect::Send + Change::Line (own echo, printed only if WireLine::new succeeded) + Change::ClearInput
```

**4. A sink reads what it needs.** The window, persistence, and later notifications and logging all implement `Sink`. `View::apply(&Workspace, &[Change])` is in `check/src/usage.rs`.

## Shape

### Where state lives (answers 1, 3)

```
tokio threads                      GTK main thread (one task: app::run)
─────────────                      ─────────────────────────────────────
transport task ──Msg::Net──►  inbox ─► Workspace::update(msg, clock, &mut Out)
 (TLS, framing,                         ├─ Session::handle(..)   per server, pure
  write queue, flood)                   └─ nav / input / selection
secrets task  ──Msg::SecretsLoaded►              │
GTK signals   ──Msg::Ui(UiAction)►               ▼
                                       Out { effects, changes }
                                         │effects → Io (dial, send, hangup, save, secrets)
                                         │changes → each Sink reads &Workspace: View, Persist, (Notify, Log)
```

- `Workspace` is the only owner of protocol and UI-visible state. It is a plain struct moved into the `app::run` future. There is no `Rc<RefCell<>>` and no lock. Widgets cannot reach it: they send `Msg::Ui`. A signal that fires while the view applies a change only enqueues a message, so re-entrancy cannot happen. The messages are idempotent (`Select(already focused)` is a no-op), so the echo of a programmatic selection costs nothing. Per **separate-before-serializing-shared-state**: nothing is shared, so nothing is serialized.
- Socket tasks own bytes only: connect, TLS, framing, an ordered write queue, flood pacing. They hold no nick, no caps, no channels.
- `Session::handle(input, &Ctx, &mut Out)` is the reducer. It never reads a clock, a socket, a widget or a secret store directly; `Ctx` carries the clock, the secret cache and the focus. A test drives it by constructing `Ctx`.
- Changes carry ids, plus the three payloads that cannot be pulled afterwards (`Line`, `Members` ops, `SetInput`). The sink pulls the rest from `&Workspace`. So the workspace has one copy of each fact, and the view is derived. This is the "notification plus pull" shape: no diff engine, no snapshots, no GObject properties on the model.
- Throughput: `app::run` drains up to 256 queued messages per iteration, then runs effects and applies sinks once. A ZNC playback burst is one view update pass, not thousands.

### Crate split (answers the "importable internals" red flag)

```
Cargo workspace
  crates/core   textual-core   irc/{message,casemap,isupport,registration}, model/, input/,
                               config/schema, line, style/            deps: chrono, md5, plist, serde, toml, uuid, base64
                                                                       NOT: gtk, adw, glib, gio, webkit6, tokio
  .             textual-gtk    app, ui/, io/{transport,secrets,store}   deps: core + gtk + tokio + rustls + oo7
```

A reducer that imports `gtk` fails to build. This is **encode-lessons-in-structure** applied to layering. Step 0 of implementation is a mechanical move of `irc/message.rs`, `line.rs` and `style/` with their tests.

### Module map

```
crates/core/src/
  ids.rs            Id<T>, ServerId, BufferId, ConnId
  irc/
    message.rs      keep; add WireLine (validated), Source
    casemap.rs      Casemap, Folded
    isupport.rs     Isupport: PREFIX (ordered), CHANMODES groups, CHANTYPES, CASEMAPPING, NETWORK, NICKLEN, STATUSMSG ...
    registration.rs CAP LS 302 / REQ / SASL PLAIN+EXTERNAL / nick-collision ladder as one pure state machine
  model/
    workspace.rs    Workspace, Msg, UiAction, Selection; routes, owns selection, nav, title, config edits
    session/        mod.rs (Session, Phase, handle), phase.rs (transitions + reconnect policy),
                    dispatch.rs (one fn per command), numerics.rs, membership.rs, print.rs (the funnel)
    buffer.rs       Buffer, Body, Counters, Scrollback
    members.rs      Members (sorted, emits MemberOp)
    out.rs          Out, Effect, Change, Sink
    nav.rs          next/prev with filter, nth_channel (pure over a flat order)
    title.rs        Title from (phase, nick, buffer)
  input/
    history.rs      History
    completion.rs   Textual's algorithm, ported from TLONicknameCompletionStatus
    command.rs      slash table: one static registry feeds dispatch and completion
    split.rs        split_for_wire
  config/
    schema.rs       ServerConfig, ChannelConfig, Prefs, SavedState (serde, defaults stripped)
    migrate.rs      version chain
  line.rs, style/   existing
src/
  main.rs           the 30 lines above
  app.rs            run(): the loop. Msg → update → effects → sinks
  io/mod.rs         Io: executes Effect; owns TransportHandle per server
  io/transport.rs   was irc/connection.rs; TCP, TLS, proxy, framing, flood queue
  io/secrets.rs     oo7: load_all, store, delete
  io/store.rs       atomic read/write of servers.toml and state.toml
  ui/               window.rs (View: Sink), sidebar.rs, members.rs, input_bar.rs, chat_view.rs (keep), probe.rs
```

### Core data (answers 1, 4, 5)

```rust
Workspace { servers: Vec<Session>, selection: Selection{focused, previous}, secrets: SecretCache, completion: Option<LastCompletion>, prefs }
Session   { config: Rc<ServerConfig>, phase: Phase, casemap, me, caps, isupport, buffers: Vec<Buffer>, later: BTreeMap<(Instant,u64), Later> }
Buffer    { id, name, key: Folded, body: Body, counters, scrollback, input: InputMemory }
Body      = Console | Channel(ChannelState{ join, topic, modes, members, names_in_flight }) | Query(QueryState)
```

- `Body` is a sum type. Members exist only on channels, topics only on channels. There is no `Option<Members>` that is `None` for queries.
- `Buffer` carries the persistent id (`BufferId` = the channel's config UUID, or a fresh UUID for a query). Selection restores by id.
- `buffers` order is the sidebar order: console, channels, then queries. `Session::insert_position` enforces "channels above queries" once, and drag-reorder reuses it.
- A server row in the sidebar is its console buffer. Selecting the server shows the console.
- **Users are not a table.** `Member { nick, key, modes, rank, weight }` lives in the channel. Away, host and ident are updated by walking the session's channels (tens, not thousands). That removes Textual's `IRCUser` relations retain cycle and its 5-minute GC timer. Own hostmask (needed for the byte budget) is `Session.me_host: Option<String>`, learned from our own JOIN echo.
- `Members` is a sorted `Vec` with binary-search insert (as Textual does). Every mutator appends `MemberOp`s. The view splices a `gio::ListStore` with exactly those ops when the buffer is focused, and rebuilds from `Members` on selection otherwise. A 5,000-member NAMES is one `Reset`.
- `Scrollback` holds the last N `Rc<Line>` per buffer (the model's log). `ChatView` becomes a cache of it. A view is created lazily when a buffer is first shown, filled from `scrollback`, and dropped by an LRU of about 16. Dropping is lossless. Phase 3's history search and on-disk logs sit behind the same type.

### Casemapping (answer 4)

`Casemap::fold(&str) -> Folded` is the only constructor of `Folded`. Every key is a `Folded`: buffer keys, member keys, the "is this me" test (`casemap.eq(nick, me)`). A map lookup with a raw `&str` does not type-check. `Casemap` defaults to `Rfc1459`, switches on the `CASEMAPPING` token in 005, and resets at disconnect. Because 005 arrives before any membership exists, no rekeying is needed; if a server changes it later, `Session::rekey()` rebuilds the three maps. `Line::from_message` takes `&Identity { me, casemap }` instead of `own_nick: &str`, replacing its `eq_ignore_ascii_case`. Test vectors: `[]\~` against `{}|^`, and the strict variant for `~`/`^`. Per **type-system-discipline**.

### Connection lifecycle (answer 2)

`Phase` is one enum, so illegal combinations (online with no socket) do not exist:

```
Idle{why, retry} ──Connect / retry due──► Connecting(link) ──Opened──► Registering(link, reg)
     ▲                                          │Closed                      │001            │Closed/watchdog 240 s
     │                                          ▼                            ▼               ▼
     └────────────── Closed(reason) ◄───── (any live phase)      Online(link, online) ──► Idle
                                                                       │ Quit
                                                                       ▼
                                                              Quitting(link, until) ──Closed or until──► Idle{UserQuit, retry: None}
```

- `Closed(reason)` in a live phase goes to `Idle { why, retry: policy(why, attempt) }`. `Disconnect::retryable()` returns false for `UserQuit`, `Banned`, `BadPassword`, `BadCertificate`, and ERROR text for excess flood or SendQ. Otherwise the next try is in `min(5 s * 2^attempt, 5 min)`. The attempt counter resets only after the session stayed Online for 60 s, so a server that accepts then drops us still backs off. Textual's fixed 20 s timer is replaced. Address round-robin is kept.
- Every transport event carries `(ServerId, ConnId)`. An event whose `ConnId` is not the current link's is dropped. A late `Closed` from the previous socket cannot tear down the new one. `Effect::Hangup` for a stale conn is a no-op in `Io`. Both are repeat-safe, per **make-operations-idempotent**.
- Timers are derived, not scheduled. `Session::next_wake()` is the minimum of the phase deadline (retry due, 240 s watchdog, 270 s idle PING, 360 s idle drop, quit grace) and `later` (kick rejoin after 3 s, NickServ wait 90 s). The loop sleeps until `Workspace::next_wake()` and sends `Msg::Wake`. Nothing is cancelled when a phase ends because nothing was registered. Time is injected, so the 270 s and 360 s paths are tested in microseconds.
- Registration is a pure sub-machine (`irc/registration.rs`) living inside `Phase::Registering`. It fixes the three Textual defects: REQs go out once, after the final `CAP LS` chunk (the `*` continuation is parsed), batched into as few lines as fit 512 bytes. SASL internals are a separate `Sasl` enum instead of markers mixed into the pending-caps list. The 433/432/437 ladder is alt nicks, then `_` padding up to NICKLEN, then `0`. SASL PLAIN keeps Textual's authzid and authcid equal to the username, with the NickServ password. A SASL failure continues to `CAP END`, as Textual does.
- After 001: `on_connect` commands, then the autojoin gate (`Open` at once if SASL succeeded or the server does not wait for NickServ; else wait for the NickServ identified notice or 90 s). Autojoin is chunked JOIN lines under the byte budget with keys on their own lines; the transport's flood queue paces them.
- **Moved out of `connection.rs` and why.** Registration, PING reply, 433 retry and autojoin move into the core because they read config, secrets, ISUPPORT and time, and because reconnect, SASL and config edits need to see them. `connection.rs` becomes `io/transport.rs`: dial with timeout, rustls handshake, line framing (the existing `LineReader` stays), an ordered write queue with `Priority::Immediate` bypass, optional flood pacing, and `Opened`/`Line`/`Closed` events. It parses `Message` off the main thread and forwards the raw line too for a raw-log view. It accepts `WireLine` only, so a line that cannot be sent never reaches it; the phase-1 `Event::Refused` disappears because refusal is decided where the line is built.
- **Config edits reaching a live connection** (`SessionInput::ConfigChanged(Rc<ServerConfig>)`, idempotent). The session diffs old against new. Applied at once: name, channel adds (buffer created, JOIN if online and `auto_join`), channel removals (PART, buffer closed), per-channel flags, nick change (sends `NICK` if Online), highlights and ignores. Applied at next connect: addresses, TLS, SASL, proxy, flood. In that case the session marks `reconnect_suggested`, which the sidebar shows as a row hint. The edit never reconnects by itself.
- Sleep, resume and `gio::NetworkMonitor` are `Msg::Suspend/Resume/Network`. They become `SessionInput::Network` plus `Quit`/`Connect`, so Textual's wake staggering is a `later` entry.

### Message to state to UI (answer 3)

`Session::handle(Net(conn, Line{message}))`:

1. Drop if the conn is stale. Touch `last_rx`.
2. Route by phase: `Registering` gives the line to the registration machine first; 001 moves to `Online` and flushes the autojoin gate.
3. `dispatch.rs` has one function per command or numeric. It mutates state (membership, topic, modes, away, counters) and calls `print(dest, line)` or nothing. For QUIT and NICK it first computes the buffers the nick appears in, prints into each, then mutates. That replaces `Destination::ChannelsOf` guessing with real membership.
4. `print.rs` is the single funnel the research describes. It resolves a destination to a buffer: a channel we are in, a query created on demand for an incoming PM (own echoed PRIVMSG routes to the target's query), or the console for server notices and numerics. A public message for a channel we are not in is dropped. It computes `highlight` (pure, from own nick and later the keyword list), appends to `scrollback`, applies the counter rule with `ctx.focus`, and emits `Change::Line { attention }` followed by `Change::Counters`. Counting needs no render result, so nothing waits on the web view.
5. Counter rule (`Counters::bump`): nothing for the console; nothing if the buffer is focused and the window is active; else `unread += 1`, and `highlights += 1` for a highlight. Reset on selection, on window activation for the focused buffer, and on mark-all-read.
6. Own sends: `WireLine::new` runs first. On success, emit `Effect::Send` and, unless `echo-message` is on, print the echo in the same step. On refusal, print a debug line and do not clear the input. This delivers phase 1's deferred "echo only after the connection accepts it". "Accepted" means the line passed grammar and length checks. A socket error later is a disconnect, which is already visible.

### Sidebar, member list, chat views, title (answer 5)

- Sidebar: a `gtk::ListView` over `gtk::TreeListModel` of `gio::ListStore<BoxedAnyObject<Row>>`. Root rows are servers; the child model of a server row is its buffer rows. `TreeExpander` in the factory. `gtk::SingleSelection` sends `UiAction::Select`. Rows are `BoxedAnyObject` of plain Rust row data and replaced on `Change::BufferChanged`/`Counters` through `items_changed`. No GObject subclass until a profile asks for one. The sidebar expanded flag is `ServerConfig.sidebar_expanded`.
- Members: a second `ListView` over a `gio::ListStore<BoxedAnyObject<MemberRow>>`, driven by `MemberOp`.
- Chat views: `ui/window.rs` keeps `HashMap<BufferId, ChatView>` in a `gtk::Stack`. `ChatView::new` takes the buffer's kind and name instead of the hard-coded `"channel"`. New method `ChatView::reload(style, lines)` serves the later theme switch. Switching calls the existing `notifyDidBecomeVisible/Hidden` JS.
- Title: `Workspace::title(buffer)` is a pure function of phase, nick, away, network name, and for channels the user count and mode string. The view calls it when the focused buffer is in the change set. The topic goes to `ChatView::set_topic`, not the title (as Textual).
- Selection: `Selection { focused, previous }`. Textual's multi-pane group (up to 6 panes) is not in phase 2; the struct is where `visible: Vec<BufferId>` would go. Cmd+1..0 is `nth_channel`. Next/previous with the All / Active / Unread / Highlight filters, optionally within one server, is `nav::step` over the flattened order.
- Stable CSS classes: window `.tx-window`; sidebar `.tx-sidebar`, rows `.tx-server-row` / `.tx-buffer-row` plus `.kind-channel` / `.kind-query`, state `.state-joined` / `.state-parted` / `.state-error` / `.state-offline`, `.has-unread`, `.has-highlight`, badge `.tx-badge`; `.tx-member-list`, `.tx-member-row` plus `.rank-owner|admin|op|halfop|voice`, `.is-away`; `.tx-input`, `.tx-topic`. These are listed once in `ui/classes.rs` as constants and nothing else writes a class string.

### Input (answer 6)

All of it is pure functions in `crates/core/src/input`, driven by `UiAction`s.

- **History**: per buffer by default (`HistoryScope::PerBuffer`, switchable to global), at most 100 entries, consecutive duplicates skipped (Textual's check is broken), the unsent draft held in a separate `stash` instead of being pushed as an entry. `Up`/`Down` reach the model only when the caret is on the first or last line of the multi-line field (the widget decides; `Ctrl+P`/`Ctrl+N` always send). A per-buffer draft is saved on `Select { leaving_draft }` and restored via `Change::SetInput`.
- **Completion**: `complete(&CompletionSource, last, text, caret, backwards)`, a direct port of the research notes. It scans back to whitespace or comma; `/` at the start completes commands; `@` is stripped and restored; `#` completes channels (current first); otherwise nicks, only in a channel. Members come from `Members` with their `weight`. Weights decay by half per minute and are bumped from `print` (+20/+5 when the line mentions us, +4/+1 otherwise). Matching is a case-insensitive prefix match under the server casemap, including the leading `^[]-_`{}\` trim. Server service names and the network name are appended last. State is `LastCompletion`, reused only when text and caret equal what the last Tab left, so it needs no explicit invalidation. Suffix rules (": " at line start, one space elsewhere) are included.
- **Slash commands**: one static table `COMMANDS: &[CommandDef { name, aliases, target: Target, run: fn(&mut Session, Args, &mut Out) }]` in `input/command.rs`. Dispatch and completion both read it, so adding a command is one row. Phase 2 set: `join part msg query me notice nick quit topic mode kick invite whois away close connect disconnect reconnect raw quote`. An unknown command is sent raw, uppercased. The console treats every line as a command; in a channel or query, `//x` sends `/x` and a lone `/` is text. The table is a `Vec` in a `CommandRegistry` so phase 4 plugins can add rows.
- **Splitting**: `split_for_wire(text, budget)`. Budget is `512 - len(":nick!user@host CMD target :") - 2` using the real hostmask once known, else 63 for the host part plus the max ident, not Textual's 60-char guess; ACTION subtracts 9. It never splits inside a UTF-8 sequence, prefers a space in the last 25 bytes, and reopens active bold/colour/etc. at the start of the next chunk. A property test: joined chunks equal the input, every chunk is within budget.
- **Multi-line paste**: the field is a `gtk::TextView`. Enter submits the whole text; it splits on newlines, drops empty lines, and asks for confirmation when there are more than 4 lines or 4*510 bytes: `Change::Ask(Prompt::Flood)`, answered with `Submit { confirmed: true }`. The model keeps no pending submit; the view re-sends.
- Not connected: a `Submit` to a buffer whose session is not Online emits no `ClearInput`, so the text stays in the field, and prints a debug line "Not connected" in that buffer. The wording and whether to keep the text are an open question below.

### Config and secrets (answer 7)

- **Format: TOML, two files.** `~/.config/textual-gtk/servers.toml` (what the user configured, hand-editable, diff-able) and `~/.local/state/textual-gtk/state.toml` (selection, window size, per-server `last_server_time`). They have different save policies and different owners, so a timestamp tick never rewrites a config file. GSettings was rejected for servers: it needs an installed compiled schema (breaks `cargo run` and complicates Flatpak dev), has no good shape for nested lists of servers/channels, and cannot be hand-edited. Flat app preferences may move to GSettings in phase 3 when the Preferences window wants `settings.bind`. This is logged in `DECISIONS.md` as a default Michael can override.
- **Schema** mirrors Textual's per-client and per-channel fields (the brief's list), as serde structs with `#[serde(default, skip_serializing_if = "is_default")]` on every field. That is Textual's "defaults stripped" format, and it lets a default change without rewriting users' files. Unknown keys are kept (round-trip through `toml_edit::DocumentMut`) so a newer file is not damaged by an older build.
- **Secrets: `oo7`.** Pure Rust, async, speaks both the Secret Service D-Bus API and the xdg-desktop-portal Secrets portal, so it works unchanged inside a Flatpak. It is chosen over `secret-service` for that reason. Each secret has attributes `{application: textual-gtk, kind: server|nickserv|channel-key|proxy, owner: <uuid>}`, which mirrors Textual's `textual.server.<serverId>` keys. `ServerConfig` has no password fields. `SecretRef` names the owner; `SecretCache` is filled once at start by a single `search_items`, updated on edit.
  - A locked or missing keyring must not block the window. If loading fails, `SecretsLoaded(Err)` arrives and the workspace marks `secrets_loaded` anyway, prints one console notice per server that needs a password, and autoconnect proceeds with what it has. The session starts only after `Msg::SecretsLoaded`, either way.
  - `Secret` has no `Debug`, `Display` or serde impl. The `Registration` machine reads it only to build AUTHENTICATE and PASS lines. Raw-log view redacts those lines by command name.
- **TLS: rustls with `rustls-platform-verifier`**, not webpki-roots. Self-hosted bouncers use private CAs and user-installed roots; the platform verifier sees them and the bundled Mozilla list does not. Per server: `verify_chain` (Textual's `validateServerCertificateChain`) and `trusted_fingerprints` (SHA-256). A chain failure ends as `Disconnect::BadCertificate { sha256 }`, not retried, and raises `Change::Ask(Prompt::TrustCertificate)`; "Trust" appends the fingerprint through an `EditServer` and redials. Client certificates (SASL EXTERNAL) come later; `TlsPolicy` and `Dial` have room.
- **Saving.** The core never touches files. Config changes emit `Effect::SaveConfig` (debounced 500 ms in `Io`); `Effect::SaveState` is requested by selection change and `last_server_time` and written at most every 300 s plus at quit. Both writes go to `file.tmp` then rename, so a crash leaves the old file. At quit, `Workspace` waits for all sessions to be `Idle` (or 3 s) and emits `Effect::Quit`; `Io` flushes both files first. `Workspace::to_config()` and `to_state()` are the only serialization entry points.
- **Migration.** `schema_version` at the top of each file. `config/migrate.rs` is a chain `v1→v2→…` over `toml::Table`, run before deserializing, with a `.bak` of the file kept the first time a migration rewrites it. A file with a newer version than the build knows is loaded read-only and never rewritten, with a console notice. A Textual-for-Mac plist importer is a later `config::import` using the `plist` crate already in the tree.

### Testing (answer 8)

1. **Unit, no I/O:** casemap vectors; `Isupport` token parsing; `Members` ordering and ops; `History`; `complete` (the research's cases: `/` at start, `@`, `#`, trimmed nicks, cycling, suffix, stale state); `split_for_wire` property test; config round-trip, defaults stripped, migration fixtures.
2. **Scenario files** (the lever): `tests/scenarios/*.irc` run by one harness against `Session`/`Workspace` with the virtual clock. Seed set: registration with and without SASL; multiline `CAP LS`; 433 ladder; NAMES twice; self-JOIN twice; netsplit QUIT fan-out; NICK in several channels plus a query; rfc1459 `nick[]`; reconnect backoff and the 60 s reset; ERROR excess flood does not redial; stale `ConnId` ignored; config edit while online; unread/highlight with and without focus. Each row asserts wire output, `Change`s, and model queries. The reducer being pure is what makes this one harness enough.
3. **Transport tests with real sockets** (the existing `tests/connection.rs` grows into `tests/transport.rs`): framing and the overlong-line drop, QUIT grace, connect timeout, flood pacing, a rustls handshake against a loopback listener with an `rcgen` certificate, pinned fingerprint accepted, wrong fingerprint refused.
4. **Workspace tests:** `Msg` in, `Out` out for selection, nav filters, `nth_channel`, mark-all-read, submit paths (classify, split, flood prompt), Select saving and restoring drafts.
5. **Layering check:** the crate split is the check.
6. **E2E (`scripts/e2e.py` grows).** `TEXTUAL_GTK_PROBE` gains a `ui/probe.rs` that dumps a `ViewState` JSON (sidebar rows with classes and badges, member rows, title, input text, focused buffer) instead of only the page DOM, and accepts `Msg::Ui(UiAction)` lines from a file, because UI actions are data. Added scenarios against Ergo: two servers (two Ergo ports); join channels on both; a message in an unfocused channel raises its badge and a highlight sets the highlight class; select clears it; member list equals NAMES and follows JOIN/PART/MODE; Tab completion by action; kill the connection server-side and see backoff plus rejoin; restart the app and see servers and selection restored from disk; Ergo TLS listener with a self-signed certificate (untrusted → ask, trust → connect); SASL PLAIN against an Ergo account (numeric 900 in the console).

### Phase 3/4 hooks (shape only)

- Notifications and logging are `Sink`s over `Change::Line { attention }`; they need no change to the core. Historic-line suppression needs `Line.historic`, set from `server-time` plus the playback batch.
- ZNC playback: `SavedState.last_server_time` and `Session` already persist and carry the timestamp; BATCH handling is not in phase 2.
- Plugins: `CommandRegistry` rows; input interception is one `Workspace::submit` step.
- Selection group and split panes: `Selection`.
- Proxy, flood control, encodings: fields exist in `ServerConfig`; the transport owns them.

## Synthesis decision

Left for arena.

## Tradeoffs accepted

- We accept the reducer running on the GTK thread in exchange for a single owner with synchronous reads (title, completion, counters) and no mirror. The cost is main-thread time for a line burst. Draining 256 messages per pass and coalescing sink work bounds it; if playback of 100k lines ever shows jank, the fallback is chunking `Msg::Net` batches, not a second thread.
- We accept messages for every UI action in exchange for no re-entrancy and a scriptable UI (e2e injection). A key press costs one extra queue hop.
- We accept "changes carry ids, sinks pull" in exchange for one copy of each fact. A sink has to know which accessors to call per change; this is why `Workspace` exposes read accessors and nothing mutable.
- We accept re-sending drafts and the flood confirmation from the widget in exchange for the model holding no pending-submit state.
- We accept no `IRCUser` table. Away or host updates walk the session's channels; with tens of channels this is cheaper than the relation graph it replaces, and a user in no channel is simply forgotten. WHOIS data is not cached.
- We accept `BoxedAnyObject` rows over GObject subclasses. Property bindings and animations on rows will need a subclass later.
- We accept a two-crate workspace and a one-time file move in exchange for a build-enforced boundary.
- We accept no jitter on reconnect backoff. One client dialing a handful of different hosts has no thundering herd.
- We accept TOML plus a later GSettings split for flat preferences, instead of picking one store for everything now.
- Looks like an oversight but is not: the console has no `Counters` effect (it never counts, as Textual); the multi-pane selection group is absent; `Disconnect::retryable()` is a method, not a config table, because the policy is code.

## Alternatives considered

1. **GObject model.** `Session`, `Buffer`, `Member` as GObjects with properties; `ListModel`s are the model; the UI binds directly. Interface depth: the public surface is every property and signal of every object; callers (the view) must know which signal fires when. It hides little. Casemapped lookup, ordering and counters still need Rust structures beside the GObjects, giving two copies. Headless tests need the GObject type system. Rejected.
2. **Per-server actor on tokio owning the state, UI holds a mirror.** Deep for the UI (it just receives snapshots) and takes load off the main thread. Rejected because it has two copies of truth, and the synchronous reads (completion on Tab, title, "is this buffer focused" for unread) either go through async round trips or read a mirror that lags. Also the focus fact lives on the other side of the channel. It is the right fallback if main-thread time ever matters.
3. **Fat client object like Textual's `IRCClient`:** owns the socket via async, mutates state, calls an observer or global window. Hides the most behind the smallest interface, but couples sockets, timers and UI, so the reducer can only be tested with a fake socket and a fake window. Rejected.
4. **`Rc<RefCell<Model>>` shared with signal handlers and a callback-per-change observer list.** Simple to start. Rejected: GTK emits `selection-changed` while a store is being spliced from inside the model's own `borrow_mut`; the first programmatic selection panics or needs a re-entrancy guard flag (Textual's `ignoreOutlineViewSelectionChanges`). The message queue removes the class of bug.
5. **Keep protocol state in the tokio connection task** (extend phase 1) and send rich events (`Joined`, `Members(Vec)`) to a UI-side model. Rejected: reconnect, config edits, SASL secrets and commands that need protocol facts (`/nick`, `/mode`, message splitting by hostmask) would reach into a task over a channel; there would be two state machines.
6. **Diffs from the model** (`Change` carries full before/after snapshots, or the view diffs a snapshot). Rejected: bulk and fragile; the ops for `Members` are the one place a delta pays off.

## Implementation reconciliation

Empty until a unit closes. Rule for later: any deviation from `Out`, `Change`, `Msg`, `UiAction` or the `Phase` table is recorded here with its acceptance source before the next unit starts.

## Open questions and risks

- Is `/join` adding a `ChannelConfig` with `auto_join = true` Textual's behaviour? The brief's notes do not say. The default here is on, logged as a default.
- Does Michael want Textual's draft behaviour (unsent text pushed into history by Up/Down) or the cleaner `stash`? The design uses `stash`. It is a deliberate difference.
- Is a mid-session `Submit` while Idle best handled by keeping the text and showing the state, or by Textual's behaviour (not traced)?
- `oo7` with the `tokio` feature next to the glib main loop: confirm it works under the app's runtime handle and that the portal path works in the Flatpak. If not, `secret-service` over zbus is the fallback; `io/secrets.rs` is the only file that changes.
- `rustls-platform-verifier` on Linux needs `rustls-native-certs`-style discovery; verify it picks up `/etc/ssl/certs` on Ubuntu 24.04 and the Flatpak runtime.
- Is a per-server trusted-fingerprint list enough for private CAs, or does Michael want a trust-store import?
- Risk: `Session` grows into the new `IRCClient` (13k lines). Mitigation is the `session/` split by knowledge (phase, dispatch, numerics, membership, print), and the `COMMANDS` table. Watch file sizes.
- Risk: the workspace split touches phase 1's tests and `scripts/check.sh`. Mechanical, but it lands first, alone.
- Should `BATCH` buffering (netsplit batches, chathistory) be in phase 2? The design has room but implements none.
- Is the `later` map (kick rejoin, NickServ wait) enough, or will phase 3 need cancellable timers? Today a stale entry is checked against state when it fires, so it is harmless.

## Next implementation step

Move `irc/message.rs`, `line.rs`, `style/` and their tests into a `crates/core` workspace member with `scripts/check.sh` green, then add `casemap.rs` with its vector tests and `WireLine` in `message.rs`, before any `Session` code.
