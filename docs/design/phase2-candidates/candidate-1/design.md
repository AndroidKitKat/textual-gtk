# Candidate 1: one sans-IO world on the GTK thread

Types and signatures are in `sketch.rs` next to this file. It is std-only and passes `cargo check` (crate in `check/`).

## Problem

Phase 2 turns a one-server, one-channel proof of concept into a client with many servers, a sidebar, a member list, real input, TLS, SASL, libsecret and a saved config. Textual's IRCClient spreads one network's state over a 13k-line class, an XPC socket, a global member-list queue and direct calls into the main window, and it has known bugs: no CASEMAPPING, broken CAP LS 302, no backoff, NAMES early return. The shape is not obvious for three reasons. The GTK main loop is single-threaded while sockets run on tokio, so state has to sit on one side. The UI needs incremental ListModel updates, not redraws. And the phase-1 `connection.rs` already owns part of the protocol (registration, 433, autojoin), which phase 2 has to either grow or take back. Constraints carried over: `Message` and its parser stay, `Line::render` and the style pipeline stay, `ChatView` keeps its replay-after-reload contract, and widgets need stable CSS class names for the later "Textual for Mac" stylesheet.

## Usage (caller's view)

There is one way in and one way out. The shell feeds an `Input` to `World::apply`, then draws the returned `Change`s and runs the returned `Effect`s.

```rust
// src/ui/app.rs: the whole bridge between tokio, GTK and the model.
fn dispatch(app: &App, input: Input) {
    app.queue.borrow_mut().push_back(input);
    if app.dispatching.replace(true) { return; }        // re-entrant signal: queued
    while let Some(input) = app.queue.borrow_mut().pop_front() {
        let out = app.world.borrow_mut().apply(input, Instant::now());
        let world = app.world.borrow();
        for change in &out.changes { app.ui.project(&world, change); }
        drop(world);
        for effect in out.effects { app.execute(effect); }
    }
    app.dispatching.set(false);
}

// A transport's events, forwarded from tokio (as phase 1 does today).
glib::spawn_future_local(async move {
    while let Some(event) = events.recv().await {
        dispatch(&app, Input::Transport { server, epoch, event });
    }
});

// Return in the input bar.
dispatch(&app, Input::Typed { buffer, text, as_action: false, confirmed: false });
```

The shell's projection never decides anything. It re-reads rows and patches models.

```rust
fn project(&self, world: &World, change: &Change) {
    match change {
        Change::BufferAdded { id, parent, index } => self.sidebar.insert(*id, *parent, *index, world.row(*id)),
        Change::BufferChanged(id) => self.sidebar.refresh(*id, world.row(*id)),   // badge, label, .inactive
        Change::Print { buffer, line } => self.views.append(*buffer, line),       // Line::render + ChatView
        Change::Members { buffer, diff } if Some(*buffer) == world.focus() => self.members.apply(diff, world),
        Change::Focus(id) => self.sidebar.select(*id),
        _ => {}
    }
    self.title.set(world.title());   // derived on every pass, never synced
}
```

Protocol tests feed lines and check what the client would send. They need no GTK, no socket and no clock.

```rust
#[test]
fn cap_ls_302_waits_for_the_last_line_and_batches_req() {
    let mut t = Script::new(server("ergo").sasl_plain("hunter2"));
    t.connected();
    t.sent(["CAP LS 302", "NICK textual", "USER textual 0 * :Textual"]);
    t.recv(":ergo CAP * LS * :multi-prefix sasl=PLAIN,EXTERNAL");
    t.sent_nothing();
    t.recv(":ergo CAP * LS :server-time echo-message");
    t.sent(["CAP REQ :echo-message multi-prefix sasl server-time"]);
    t.recv(":ergo CAP textual ACK :echo-message multi-prefix sasl server-time");
    t.sent(["AUTHENTICATE PLAIN"]);
    t.recv("AUTHENTICATE +");
    t.sent(["AUTHENTICATE dGV4dHVhbAB0ZXh0dWFsAGh1bnRlcjI="]);
    t.recv(":ergo 904 textual :SASL authentication failed");
    t.sent(["CAP END"]);                       // failure continues, as in Textual
    t.console_contains("SASL authentication failed");
}
```

`Script` wraps a `World` with one configured server. Its `connected` and `recv` methods build `Input::Transport`. Its `sent` method drains `Effect::Send`. Its `console_contains` method reads `Change::Print` for the console buffer. Fixture transcripts (`tests/fixtures/*.irc`, lines `S: ...` and `C: ...`) replay through the same type, so a recorded Libera or ZNC session becomes a test.

## Shape

**Ownership and threads.** All client state is one `World` value on the GTK main thread, written only by `World::apply`. It holds the config, runtime UI state, one `Network` per server, every buffer, unread counts and focus. Tokio runs one transport task per connection attempt. A transport holds no IRC state. It owns DNS, TCP, rustls, line framing (the existing `LineReader`), PONG replies, the idle PING at 270 s and close at 360 s, the flood-control token bucket, the 5 s QUIT grace, and loading secrets before it dials. So nothing is copied across threads. The transport sends parsed `Message`s in and receives validated `Message`s out (per separate-before-serializing-shared-state: the sharing is removed, not locked). PONG stays on the tokio side because it is stateless and keeps the link alive while the main thread is busy rendering. The phase-1 registration, 433 retry and autojoin move out of `connection.rs` into the model, because CAP, SASL and nick fallback need config and session state.

**Data structures.**

- `Network` lives for as long as the server is configured. Its `Link` enum is the lifecycle: `Offline`, `Connecting`, `Registering { reg, session }`, `Online { session }`, `Waiting { until, last }`, `Quitting { session, then }`. The `Session` exists only inside `Registering`, `Online` and `Quitting`. Disconnecting drops the session, so the reset cannot forget a field the way Textual's `resetAllPropertyValues` can (per make-illegal-states-unrepresentable in type-system-discipline). `Registration`, which holds the CAP phase, the SASL phase, the nick attempt and the one-shot `Secrets`, exists only while registering. So the secrets are dropped at 001.
- `Epoch` goes up on every connect attempt. Transport events, timer tokens and send effects all carry it, and `apply` ignores anything from an old epoch. Timers are never cancelled. A stale `Reconnect` or `RegistrationWatchdog` firing is simply a no-op, and every transition is a no-op in a phase where it does not apply, so doubled inputs converge (per make-operations-idempotent).
- `Session` holds the nick, our hostmask, the acked caps, `Isupport`, `channels: HashMap<Key, ChannelState>` and a network-wide `users: HashMap<Key, User>`, where each user has a reverse set of channel keys. A user is removed as soon as no channel or open query holds it. Textual's 5-minute GC timer is not needed because there is no retain cycle to break.
- `Key` is a casefolded name. Its only constructor is `Casemapping::fold`, so a map keyed by nick or channel cannot be looked up with a raw `&str`. `Name { shown, key }` keeps the server's spelling for display. CASEMAPPING defaults to rfc1459 before 005. If a 005 changes it, `Session::rekey` rebuilds every key in one place. Keys never cross sessions.
- `Members` per channel: `modes: HashMap<Key, ModeBits>` plus `order: Vec<SortKey>`, always sorted by (highest PREFIX index, folded nick). `ModeBits` is a bitset over the ISUPPORT PREFIX index, so the highest mode is `trailing_zeros`. That removes Textual's rank-sorted `modes` string invariant and its hard-coded `qOao`. Every mutation returns a positional `MemberDiff` (`Insert`, `Remove`, `Update`, `Move`, `Reset`) that the shell applies to a `gio::ListStore` with `splice`. The sort policy lives in pure code where it is tested, and GTK does no sorting.
- `Buffers` is a flat map from `BufferId` to `Buffer { server, kind, unread, highlights }`. A console's `children` vector gives the sidebar order, with channels inserted before the first query (Textual's rule). `BufferId` is a process-unique counter, so a stale id held by a GTK closure resolves to nothing.

**The receive path, socket line to pixel, in three files.**

1. `irc/transport.rs` frames and parses a line, then sends `TransportEvent::Line(message)`.
2. `model/session.rs`, through `Session::handle`. It resolves the route first (`Route::Buffer`, `Console`, `Query(name)` for an incoming PM, `Drop` for a public message to a channel we are not in), then mutates state. QUIT and NICK print to the channels the nick was in before the update. It calls `line::describe`, which is today's `Line::from_message` minus routing, to build the `Line`, and decides highlight against our nick and `highlight_words`. `World::print` is the one funnel. It opens a query buffer if needed, counts unread and highlight with `buffers::counts` (nothing for consoles, nothing for the focused buffer in an active window), and emits `Change::Print` followed by `Change::BufferChanged`. The counts are decided before render because our highlight decision is pure and comes before render. Textual bumps after render only because its renderer makes that decision. The badge still updates after the line is appended, since the shell applies changes in order.
3. `ui/app.rs` projects. `Print` goes to `views.append`, which calls `Line::render(&style)` and then `ChatView::append`.

**The send path.** `Input::Typed` goes to `input::plan`, a pure function. On the console, every line is a command and the `/` is optional. In a channel or query, `/cmd` runs, `//x` sends "/x", and a lone `/` is text. Input over 4 lines or 4×510 bytes returns `Plan::Confirm` first, and the shell shows an `AdwAlertDialog` and re-dispatches with `confirmed: true`. Text is split per line with `input::split`. The budget comes from `text_budget`, which uses our real hostmask once we have seen it and Textual's 60-byte guess before that, and subtracts 9 bytes for the ACTION wrapper. Splits fall on char boundaries, never inside a colour code, and prefer a space within 25 bytes of the limit. Every chunk passes `to_wire` in the model, so the phase-1 `Refused` event is gone and nothing is printed that was not sent. With `echo-message` acked, nothing is printed locally and the echo prints with `from_self`. Without it, the line prints at send time. Slash commands come from one table, `input::COMMANDS`, which both dispatch and completion read (no hand-synced list). Unknown commands go out raw as `UPPERCMD rest`, as in Textual.

**Lifecycle and reconnect.**

- `Connect` moves the link to `Connecting`, bumps the epoch, and emits `Dial` and a 240 s `RegistrationWatchdog`. `Connected` starts registration. `Registration::start` sends `CAP LS 302`, then `PASS` (if a server password is set), `NICK`, `USER`.
- CAP collects every `LS *` continuation line until the last one. It then sends one or more `CAP REQ` lines, packed to 510 bytes. A NAK on a batch re-requests its caps one by one. If `sasl` was acked and `Auth` asks for it, AUTHENTICATE runs before `CAP END`. A 908 list with another usable mechanism gets one retry. Otherwise 902, 904, 905 or 906 end in `SaslPhase::Failed`. Registration then continues with `CAP END`, as Textual does, and the console prints why. If `autojoin_waits_for_identify` is set, autojoin waits for the 90 s `IdentifyWait` timer.
- 433, 432 and 437 before 001 try the alternates, then the nick padded with `_` up to NICKLEN, then fail with `NoUsableNick`. After 001 they are printed errors.
- 001 makes the link `Online` and resets `failures`. It then runs the on-connect commands, the NickServ IDENTIFY (when `Auth::NickServ`), and autojoin. Autojoin joins the configured autojoin channels plus `rejoin`, the channels that were joined when the link dropped. Keyed and unkeyed channels go in separate JOIN lines.
- `Closed(error)` drops the session, marks channels inactive, emits `MemberDiff::Reset`, and asks `reconnect::next_attempt`. The delay starts at 5 s and doubles to a 300 s cap, with ±20 % jitter derived from (server id, failures), so tests are deterministic. The link does not reconnect after a user quit, a bad certificate, a ban, "Excess Flood" or "SendQ", or when `auto_reconnect` is off. Each attempt moves to the next address round-robin. `NetworkAvailable(true)` from `gio::NetworkMonitor` skips any pending wait.
- `QuitApp` sends QUIT on every live link, then emits `Exit` once all links are offline or the 3 s `QuitDeadline` fires. This keeps phase 1's clean-quit behaviour.
- Config edits go through `EditServer(new)`. `config::live_changes(old, new)` is pure. Channel additions and removals take effect now (create or remove the buffer, JOIN or PART if online). Addresses, TLS, auth and username apply at the next connect. A changed nick does not send NICK by itself, as in Textual. The edit marks the config dirty and emits `SaveConfig`.

**UI projection (GTK side, `src/ui/`).**

- The sidebar is an `AdwOverlaySplitView` holding a `gtk::ListView` over a `gtk::TreeListModel`. The root is a `gio::ListStore` of `BufferObject` consoles, and each console has a child `ListStore`. `BufferObject` is a small GObject with properties (label, kind, active, unread, highlights, link). It is filled only from `World::row`, so the shell writes it and never reads it back as truth. The shell keeps a `HashMap<BufferId, BufferObject>` index. Expanded state goes back as `Input::Expanded` and is saved in `state.toml`.
- Selection is a `gtk::SingleSelection`. A user click sends `Input::Focus`. A focus move the model makes (a new query from `/query`, closing the focused buffer, restoring the saved selection) comes back as `Change::Focus`, and the shell then calls `set_selected`. Setting the row that is already selected emits nothing, so the loop ends. Multi-selection split panes are left for phase 3. `Focus(Option<BufferId>)` can become a `Selection { group, focused }` without touching the protocol code, because only `print` and `buffers::counts` read focus.
- The member list is one `gtk::ListView` over one `gio::ListStore<MemberObject>`. On a focus change the shell splices in `world.members(focused)`. After that it applies only the diffs for the focused buffer. The pane collapses for consoles and queries.
- Chat views: one `ChatView` per buffer, created the first time the buffer is focused, then kept in a `gtk::Stack` for as long as the buffer exists (Textual keeps one log controller per tree item). The 1000-line replay deque moves out of `ChatView::Inner` into a per-buffer `Scrollback` owned by the shell's view registry. Prints to an unopened buffer land there, and a new view replays from it the same way a reloaded page does today. That is one store with one contract.
- The title is derived every pass from `World::title()`, built from link phase, nick, network, then channel name, member count and modes, or the query nick and host. The topic goes to the chat view's topic bar, not the title, as in Textual.
- CSS classes come from one function, `ui::sidebar::row_classes(&BufferRow)`. It yields `server-row`, `channel-row`, `query-row`, `inactive`, `unread`, `highlight`, plus `unread-badge`. The panes are `server-list`, `member-list`, `chat-view` and `input-bar`, and member rows get `member-row` plus `member-op` and similar per mode. One function means the Mac stylesheet has one list to match.
- Input bar: a `gtk::TextView` that grows to a few lines. Enter sends and Shift+Enter inserts a newline. Up and Down go through history only on the first or last line, and Ctrl+P and Ctrl+N always do. Tab and Shift+Tab are window-level shortcuts. `input::History` lives per buffer, which also gives per-buffer drafts, and drops Textual's duplicate and draft-pushing bugs. `input::Completion` follows Textual's algorithm and is fed by `World::completion_source(buffer)`: weight-ordered members (weights halve each minute, decayed lazily on read), channels, commands from `COMMANDS`, service names and the network name.

**Config and secrets.** TOML through serde. `~/.config/textual-gtk/config.toml` holds the servers and is written only on user edits, atomically (write a temp file, fsync, rename). `~/.local/state/textual-gtk/state.toml` holds expanded rows, the selection, pane visibility, window size and `last_server_time`. It is saved at quit and on the 300 s `SaveState` timer when dirty. Splitting the two files means runtime saves never rewrite a file the user may have edited by hand. GSettings lost because a list of nested server records fits it badly (relocatable schemas or a JSON string), `cargo run` needs a compiled schema installed, and dconf cannot be reviewed as a diff. `config.version` plus a `migrate(n → n+1)` chain covers format changes. A later importer for Textual's plist export fits the same entry point, and the `plist` crate is already a dependency. Secrets use `oo7`: it is pure Rust, async on tokio, and works with both the Secret Service and the Flatpak portal backend. The attributes are `{app, kind, id}`, using Textual's keys (`ServerPassword(address)`, `Account(server)`, `ChannelKey(channel)`). The transport loads secrets before it dials and hands them over in `Connected`. `World` never stores them, and `Secret`'s `Debug` output hides the value. The add-server dialog stores secrets through `oo7` on tokio before it dispatches `AddServer`. `Auth` is explicit (`None`, `SaslPlain`, `SaslExternal`, `NickServ`). Textual instead infers PLAIN from a NickServ password being set.

**TLS.** `tokio-rustls` with `rustls-platform-verifier`, which uses the system trust store, so enterprise and local CAs work where webpki-roots would not. `Tls::Unverified` is Textual's `validateServerCertificateChain = NO`. A rejected chain ends with `LinkError::BadCertificate { sha256 }` and no reconnect. The console says how to proceed. The trust prompt and pinned fingerprints come in phase 3, and the error already carries the fingerprint they need.

**What it deliberately does not do.** No locks, no shared `Arc` state, no GObject inside the model, no async inside the model, and no timers the model has to cancel. ZNC playback, logging and notifications are not in phase 2. Their hooks: `last_server_time` is already saved, `Change::Print` is the one place a logger or notifier subscribes, and `batch` is acked so playback batches can be told apart later.

**Interface depth.** The public surface is `World::new`, `World::apply`, eight read methods and the `Input`, `Change` and `Effect` enums. Behind it sit CAP 302, SASL, nick fallback, ISUPPORT, casemapping, membership and NAMES/WHO, routing, query creation, unread rules, reconnect policy, input planning and splitting. The shell cannot change state except through `apply`, so a change that looks right from `ui/` cannot bypass a protocol rule.

**NAMES and WHO.** 353 lines collect into `ChannelState::names`. 366 swaps them in whole and emits `MemberDiff::Reset`. A later `/names` refreshes the list too, which fixes Textual's early return and its ignore-after-the-first-reply bug. With `userhost-in-names`, it also updates user and host. After 366, a hidden `WHO #chan` (WHOX when offered) goes out for channels under 500 members, and only when `away-notify` is absent. Replies to it are recorded in `hidden_who` and not printed. Self JOIN on a channel we are already in is a no-op (bouncer re-join). Self PART or KICK marks the buffer inactive and clears the members, and the buffer stays.

## Synthesis decision

Left for arena.

## Tradeoffs accepted

- We accept protocol work on the GTK main thread in exchange for one owner and no copies. Handling a line is microseconds next to a WebKit render, and PONG stays on tokio, so a slow frame cannot time out the link.
- We accept that `World` is a large module, in exchange for a single place where state changes. It is split by knowledge (`registration`, `session`, `members`, `buffers`, `network`) with `apply` as a thin match, and each submodule has its own tests.
- We accept that rows are re-read rather than diffed for the sidebar and title, in exchange for no sync code. Only member lists and prints carry payloads, because a 2000-member channel cannot be re-read on every JOIN.
- We accept counting unread before render, which differs from Textual's order on paper, in exchange for a pure, tested highlight decision. What the user sees is the same, since the badge refresh comes after the append.
- We accept an explicit `Auth` enum that differs from Textual's config, in exchange for a migration that is one function and no guessing at connect time.

## Alternatives considered

- **An actor per network on tokio that owns the session and sends diffs to a GTK-side mirror.** This is closest to Textual's XPC split. It lost because the member list and channel state exist twice, and every diff type is a sync contract between the two copies. The reducer also becomes async to test. Its interface looks deeper, but it hides less, because the UI mirror has to re-implement ordering and routing.
- **GObject-native model.** `ChannelObject` owns a member `ListStore`, and handlers mutate GObjects that widgets bind to directly. There is no projection layer, and that is the appeal. It lost because protocol logic would need GObject subclasses and `RefCell` borrows in every handler, tests would need a GLib type system, and casemapping maps would sit beside the ListStores as a second index.
- **`Arc<Mutex<World>>` shared by the transports and the UI.** It lost because each transport would mutate state under a lock while the UI reads half-applied updates. And it still needs change notification, which is the `Change` list this design already has.
- **GSettings instead of TOML** lost for the reasons in Shape.

## Implementation reconciliation

Empty until implementation starts.

## Open questions and risks

- Textual rejoins after a reconnect. Does 4f09312 rejoin only the `autoJoin` channels, or every channel that was joined? This design rejoins both, and `performAutoJoin` should be checked before implementing.
- Should `/join` of a channel not in the config add it to `config.toml` with `autojoin = true` (persisted), or keep it session-only until the user opts in? This design adds it, which is what I believe Textual does. That needs checking in `IRCClient` `joinChannel`.
- Phase 2 has no preferences dialog. Is a minimal "Add network" dialog (name, address, port, TLS, nick, password) in scope, with editing done by hand in TOML until phase 3? I'd include it, since multi-server is unusable without it.
- Splitting keeps colour codes intact but does not re-open formatting on the next chunk. Textual's `IRCColorFormat.m:470-600` was not read and may carry it across. This needs a check.
- Risk: the main thread's JS evaluation for a big NAMES-driven redraw. `Reset` re-splices up to a few thousand `MemberObject`s. It should be measured on a 2000-member channel before deciding whether to batch.
- Risk: a CASEMAPPING change after channels exist is rare but makes `rekey` touch every map. It should have its own test.

## Next implementation step

Write `model/registration.rs` and the `Script` test harness first. Use the CAP LS 302, SASL and nick-fallback transcripts above as the first tests, with `transport.rs` reduced to framing, PONG and dialing. Registration is the piece phase 1 has to give up, and every later handler test reuses the harness.

## Module map

```
src/
  irc/message.rs        keep
  irc/casemap.rs        Casemapping, Key, Name
  irc/isupport.rs       Isupport (005 merge, PREFIX ranks, CHANMODES)
  irc/transport.rs      was connection.rs: dial, rustls, framing, PONG, idle ping, flood bucket, QUIT grace, secrets load
  model/mod.rs          World, Input, Outcome, Change, Effect, read model; apply = thin match
  model/network.rs      Network, Link, Epoch handling, reconnect::next_attempt
  model/registration.rs CAP 302, SASL, nick fallback (pure)
  model/session.rs      Session, receive switch, routing, NAMES/WHO, users
  model/members.rs      Members, ModeBits, MemberDiff
  model/buffers.rs      Buffers, unread rules
  input/commands.rs     COMMANDS table, plan()
  input/split.rs        text_budget, split
  input/history.rs      History
  input/completion.rs   Completion
  line.rs               keep; from_message → describe (no routing)
  config/mod.rs         Config, UiState, load/save/migrate, live_changes
  config/secrets.rs     oo7 wrapper, SecretKey, Secret
  ui/app.rs             dispatch loop, effect executor (transports, timers, saves, dialogs)
  ui/sidebar.rs         BufferObject, TreeListModel, row_classes
  ui/member_list.rs     MemberObject, ListStore splice from MemberDiff
  ui/input_bar.rs       TextView, keys, history/completion glue
  ui/chat_view.rs       keep; replay deque moves to ui::Scrollback
  main.rs               load config + state, build App, run
```

## Testing

- **Model unit tests:** registration, session handlers, members ordering and diffs, casemapping folds, ISUPPORT merge, reconnect delays, input planning, splitting, history and completion. All run under `cargo test` with no GTK, no network and no clock, because `apply` takes `now`.
- **Transcript fixtures:** `tests/fixtures/*.irc` replayed through `Script`. Seed them from Ergo, from Libera (recorded read-only, with nicks scrubbed) and from a ZNC session. Cover NAMES split across several 353 lines, rfc1459 `[]`/`{}` nicks, a NAK of a batched REQ, and a 908 fallback.
- **Transport tests:** the existing `tests/connection.rs` fake server, now asserting framing, PONG, idle timeout, flood pacing and TLS against a self-signed `rcgen` certificate (Verified fails with `BadCertificate`, Unverified connects).
- **e2e (`scripts/e2e.py`):** the probe also dumps the sidebar rows, member rows and title as JSON. It gains scenarios for two Ergo listeners as two servers, SASL PLAIN against an Ergo account, TLS on Ergo's TLS port with `Unverified`, an unread badge on an unfocused channel, a member list update on JOIN/PART/MODE, and a reconnect after `docker restart`.
