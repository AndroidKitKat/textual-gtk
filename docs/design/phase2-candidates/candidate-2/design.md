# Candidate 2: one reducer per network, a dumb transport, effects into GTK

Michael's goal: turn the phase-1 proof of concept into a real multi-server IRC client that behaves like Textual, on GTK4 and libadwaita, with a core that can be tested without a window. The sketch is in `sketch.rs` next to this file; it compiles (`check/` holds the throwaway crate, `cargo check --offline` passes).

## Problem

Phase 1 proved the render pipeline. Everything else is glue in `main.rs`: one connection, one view, nick tracked in an `Rc<RefCell<String>>`, topic matched by `eq_ignore_ascii_case`. Phase 2 needs many networks, each with a connection lifecycle, CAP/SASL, ISUPPORT, channels, members, queries, and a sidebar, member list, title and input that follow all of it. Two constraints shape the design. The GTK main loop is single-threaded and owns every widget, while tokio owns the sockets. And the parts Textual got wrong (CAP LS 302, no backoff, no CASEMAPPING, the NAMES early return) are exactly the parts that are hard to see in a GUI and easy to see in a test fed recorded lines. So the protocol logic has to run with no socket, no timer and no GTK in the loop, and every piece of session state needs one owner. Existing code to keep: `irc/message.rs` (parser, `to_wire`), `line.rs` (`Line::from_message`, `render`), `style/`, `ui/chat_view.rs`. `irc/connection.rs` loses its protocol duties and becomes `irc/transport.rs`.

## Usage (caller's view)

The full call sites are in the module doc at the top of `sketch.rs`. The shape in three sentences. A `Session` per network is a plain struct on the main thread with one method, `handle(Input, now) -> Vec<Effect>`. `World` (in `ui/world.rs`) owns every `Session`, dispatches inputs from the transport pump, the input bar, the sidebar and a one-second tick, and applies the returned effects to the transport, the WebKit views, the sidebar, the member list, the title and the state file. Tests construct a `Session`, feed it lines through `Script`, and assert the effects.

Socket line to pixel:

```rust
// transport pump (glib local future) -> World
world.dispatch(net, Input::Transport { conn, event: transport::Event::Line(message) });

// World::dispatch: borrow, reduce, release, apply
let effects = self.inner.borrow_mut().slots[&net].session.handle(input, self.clock.now());
for effect in effects { self.apply(net, effect) }

// World::apply, the only match on Effect in the program
Effect::Print { buffer, line } => {
    self.view(net, buffer).append(line.render(&self.style), &line.id);
    self.sidebar.count(BufferRef { network: net, buffer }, &line, self.sidebar.focused(), self.window.is_active());
}
Effect::Members(id, change) => self.member_list.apply(BufferRef { network: net, buffer: id }, change),
```

Input field to socket:

```rust
let text = input_bar.take_text();
history.add(focus, &text);
world.dispatch(focus.network, Input::Typed { buffer: focus.buffer, text });
// Session: compose::parse -> slash command or text; compose::split to the byte
// budget; Effect::Print locally unless echo-message is on; Effect::Send per chunk.
```

A reducer test, no GTK, no socket, no timers:

```rust
let mut s = Session::new(fixtures::network("libera"));
let mut t = Script::new(&mut s);
t.user(UserAction::Connect);
let conn = t.expect_connect();
t.transport(transport::Event::Connected { secured: true });
t.expect_sent(&["CAP LS 302", "NICK tester", "USER tuser 0 * :Test User"]);
t.server(":irc CAP * LS * :sasl=PLAIN,EXTERNAL multi-prefix");
t.server(":irc CAP * LS :server-time echo-message");
t.expect_sent(&["CAP REQ :sasl multi-prefix server-time echo-message"]);
t.server(":irc CAP tester ACK :sasl multi-prefix server-time echo-message");
t.expect_sent(&["AUTHENTICATE PLAIN"]);
t.server("AUTHENTICATE +");
t.expect_sent(&["AUTHENTICATE dHVzZXIAdHVzZXIAaHVudGVyMg=="]);
t.server(":irc 904 tester :SASL authentication failed");
t.expect_sent(&["CAP END"]);              // failure continues, like Textual
t.advance(Duration::from_secs(241));      // no 001: registration watchdog
t.expect_close();
t.advance(Duration::from_secs(5));        // backoff(1)
assert_ne!(t.expect_connect(), conn);     // a new ConnectionId, next server
```

## Shape

### Module map

```
src/irc/message.rs      keep
src/irc/transport.rs    was connection.rs. TCP, TLS (rustls), line framing, writes, close grace. No protocol.
src/irc/casemap.rs      CaseMapping, NickKey, ChanKey
src/irc/isupport.rs     ISupport: 005 tokens, PREFIX ranks, CHANMODES groups, CHANTYPES, CASEMAPPING
src/irc/modes.rs        mode string -> Vec<ModeChange> against ISupport; ChannelModes
src/irc/caps.rs         Caps: LS 302 accumulation, one batched REQ, ACK/NAK/NEW/DEL
src/irc/sasl.rs         SaslState: PLAIN and EXTERNAL step function
src/session/mod.rs      Session, Phase, Input, Effect, handle()
src/session/connection.rs    lifecycle steps: connect, backoff, watchdog, ping idle, quit
src/session/registration.rs  CAP, SASL, NICK collisions, 001, 005, on-connect commands, autojoin
src/session/membership.rs    JOIN PART QUIT KICK KILL NICK MODE AWAY CHGHOST ACCOUNT 353 366 352 324 332 333 TOPIC
src/session/print.rs         destination -> BufferId, query auto-open, BATCH, server-time
src/session/typed.rs         Typed input: slash command table, split, local echo
src/compose.rs          parse(), split(), History, Completion, Candidates. Pure.
src/config/mod.rs       Config, NetworkConfig, RuntimeState, Paths, load/save/migrate (TOML)
src/config/secrets.rs   Secrets trait, Keyring (oo7), MemorySecrets
src/line.rs             keep; from_message takes a CaseMapping
src/ui/chat_view.rs     keep
src/ui/world.rs         World: Slot per network, dispatch/apply, transport pump, shutdown
src/ui/window.rs        MainWindow: AdwOverlaySplitView, header title, Stack of chat views, shortcuts
src/ui/sidebar.rs       Sidebar: TreeListModel of NetworkRow/BufferRow, focus, unread, navigate
src/ui/members.rs       MemberList: ListStore<MemberRow> + SortListModel
src/ui/input.rs         InputBar: growing TextView, key handling, calls compose
src/main.rs             load config and secrets, build World, run
tests/support/script.rs Script: fixture runner for Session
```

`session/` is one module with private submodules; only `Session`, `Input`, `Effect` and the state types are `pub`. A line is traced in three files: `transport.rs` (bytes to `Message`), `session/mod.rs` and its step file (state and effects), `world.rs` (effects to widgets). `chat_view.rs` is already there.

### Data structures first

**Identity.** `NetworkId(Uuid)` and `BufferId(Uuid)` persist across restarts (they are Textual's `uniqueIdentifier`s, and they key the libsecret items). The console buffer's id is the network id, so the console needs no second stored id. `BufferRef { network, buffer }` is what the UI holds, so a widget never holds a borrow of a session. `ConnectionId(u32)` numbers transport generations inside one session. Nicks and channel names are never compared raw: `CaseMapping::nick(&str) -> NickKey` and `::chan(&str) -> ChanKey` are the only constructors, and `channels: HashMap<ChanKey, Channel>` cannot be probed with a `NickKey` (per type-system-discipline). The default mapping is rfc1459, as the spec says, until 005 says otherwise; a change re-keys the three tables once, which in practice happens before any JOIN.

**Session state.** One `Session` per network holds `config`, `desired: Desired::{Online, Offline}`, `phase: Phase`, `isupport`, `casemap`, `nick`, `own_hostmask`, `channels`, `queries`, `buffer_order`, `batches`, `last_message_time`. `Channel` holds `status: Parted | Joining | Joined`, `topic`, `modes: ChannelModes`, `members: HashMap<NickKey, Member>`, and `names_pending` so a NAMES burst replaces the list at 366 instead of appending (Textual's early-return bug cannot happen because nothing returns per nick). `Member` keeps `modes: Vec<char>` sorted highest rank first, so the symbol to draw is `modes.first()`, with the sort maintained by the one `set_mode` path through `ISupport::prefix_for_mode`. Weights for completion are integers with a decay timestamp, as in Textual.

There is no `IRCUser` shared between channels and no reverse index. A QUIT walks the channels (dozens at most) and removes the nick from each; a NICK renames in each. The dominant access patterns are "message for channel X" (one hash lookup), "nick N in channel X" (two), "all channels containing N" (linear over channels), and "ordered members for display" (the reader sorts). None needs an index added later.

**Phase, the connection state machine.** Exactly one variant at a time, and every deadline lives inside the variant that needs it:

```
Offline{last}  --Connect-->  Connecting{conn, attempt, server, since}
Connecting     --Connected-->  Registering{conn, attempt, server, reg: Registration{caps, sasl, nick_attempt, started}}
Registering    --001-->  Online{conn, server, since, last_rx, ping_sent, autojoin}
Online         --Quit-->  Quitting{conn, since}
any live       --Closed(reason)-->  Waiting{attempt, until, last}   if desired == Online and auto_reconnect and reason reconnects
                                 or Offline{last}
Waiting        --Tick past until-->  Connecting (next server round-robin, attempt+1)
Registering    --Tick past started+240s-->  Close, then Waiting
Online         --Tick, idle 270s-->  Send PING; idle 360s -->  Close, Waiting
Quitting       --Closed or Tick past since+2s-->  Offline
```

`Desired` is separate from `Phase` so "offline because the user said so" and "offline and retrying" are two states, not a boolean read after the fact (Textual's `reconnectEnabled` was cleared by `resetAllPropertyValues` right after being set, and only worked by accident). `backoff(attempt)` is a pure table: 5, 10, 20, 40, 80, 160 s, capped at 300 s, with deterministic jitter; it resets when `Online` lasted more than a minute. `Ended::Killed` (ERROR Excess Flood or SendQ) and `Ended::BadCertificate` do not reconnect, like Textual's disconnect callback. Every transport event carries its `ConnectionId`; a stale one is dropped at the top of `handle`, so a late `Closed` from an abandoned socket cannot knock a new connection into `Waiting`. That is what makes reconnect idempotent under races (per make-operations-idempotent).

**Caps and SASL as step functions.** `Caps::on_cap(subcommand, args, want_sasl) -> CapStep` accumulates every `LS *` chunk and requests nothing until the final chunk, then emits one `Request(Vec<String>)` for every wanted cap the server offered. `sasl` goes first in that REQ; on ACK the step is `StartSasl(mech)`, and `CAP END` waits until `SaslState` reaches `Succeeded` or `Failed`. Failure continues to `CAP END` (Textual's policy, and the right one: a wrong password should land you unidentified, not offline). Mechanism choice: EXTERNAL when a client cert is configured and offered (phase 3), else PLAIN when `nick_password` is set and PLAIN is offered, authzid = authcid = `username`, as Textual does, so existing Textual users' configs keep working. `CAP NEW`/`DEL` after registration run the same `on_cap` and can start SASL again. Both are tested as tables with no `Session` at all.

### Flow

**Message to state to UI.** `Session::handle` for `Input::Transport{Line}` runs four steps in order, each a private function in its own file: `step_registration` (only in `Registering`, plus CAP NEW/DEL and the 9xx numerics later), `step_membership` (state changes, emitting `Members`, `Buffer` and `Status` effects), `step_print` (`Line::from_message` and destination resolution, emitting `Print`), then `Persist` for server-time. The reducer is pure apart from the `now` it is handed, and never reads the clock or does I/O (per boundary-discipline). The UI learns what changed through `Effect`, a closed enum applied by one `match` in `World::apply`. The state itself is readable by the UI on the same thread through `World::with_session` for the three places that need a snapshot (completion candidates, title summary, member list rebuild on focus change). Fine-grained `MembersChange` and `BufferChange` map to `gio::ListStore` operations, so the ListStores are render targets like the WebView, not a second model.

**Print path, Textual's funnel.** `step_print` resolves `Destination::Channel(name)` to a `BufferId` through `casemap.chan`; a public message to a channel we do not have joined is dropped; `Destination::Channel(nick)` for an incoming PM opens a `Query` (an `Opened` effect, then the `Print`); `Destination::ChannelsOf(nick)` fans out to every channel whose `members` contains the nick, and to the query if one exists; `Destination::Server` is the console. Lines in an open BATCH are buffered under the reference tag and released on the closing BATCH, with `received_at` from the `time` tag. With echo-message negotiated, `step_typed` emits no local `Print`; the server's echo arrives as a PRIVMSG from our own nick and `from_message`'s "mine" branch routes it to the target. Without it, the local print happens at send time. `Line.highlight` is computed before render, so unread and highlight counters bump when the `Print` is applied, which is the same decision the rendered line shows; nothing waits on the WebView.

**Sidebar and member list.** The sidebar is a `gtk::ListView` over a `gtk::TreeListModel` whose root model is a `gio::ListStore<NetworkRow>` and whose child models are each network's `gio::ListStore<BufferRow>`, expanded per `sidebar_expanded`. `gtk::SingleSelection` gives the focused item. `BufferRow` properties: `name`, `kind`, `active`, `unread`, `highlights`; the row widget binds them and toggles CSS classes `server-row`, `channel-row`, `query-row`, `active`, `unread`, `highlight`, so the Mac stylesheet needs no code. Unread counters live only in `BufferRow`, owned by the sidebar, bumped by `Sidebar::count` unless the buffer is focused and the window is active, reset on focus and on window activation. The session never sees them (one owner each, per separate-before-serializing-shared-state). `BufferChange::Opened{index}` keeps channels above queries. Phase 2 ships a single focused item; the group selection with split panes is a phase-3 feature and the `Sidebar::focused()` API is the only thing that would grow.

The member list is one `gio::ListStore<MemberRow>` under a `gtk::SortListModel` with a `CustomSorter` over `session::compare_members` (rank from PREFIX, then nick under the casemapping). On focus change `World` calls `MemberList::show(target, snapshot of members)`; afterwards only `MembersChange` effects for the shown channel apply (`Added` appends, `Removed` finds by key, `Updated` sets properties and the sorter re-sorts). One store instead of one per channel, so a hundred channels cost nothing until viewed. Changing PREFIX via 005 calls `sorter.changed()`.

**Chat views.** One `ChatView` per buffer, in `Slot::views`, created on the first `Print` or the first focus, whichever comes first, and placed in a `gtk::Stack` whose visible child follows the focused item. Views are created with `related_view` so all share one web process. A closed buffer drops its view.

**Title.** `ui::title(status, focused summary) -> (title, subtitle)` is Textual's `updateTitle` as a pure function: state prefix, nick, away marker, network; then server address for a console, name, member count and masked modes for a channel, nick and hostmask fragment for a query. It runs on `Effect::Status`, `BufferChange::Summary` for the focused buffer, and focus change. The topic stays in the WebView (`ChatView::set_topic`) on `BufferChange::Topic`.

**Input.** The input bar is a `gtk::TextView` that grows from one to six lines. Return sends, Shift+Return inserts a newline, Up and Down walk history only when the caret is on the first or last line (Ctrl+P and Ctrl+N always), Tab and Shift+Tab complete. All logic is in `compose`:

- `parse(line, kind)`: console lines are raw commands with or without a slash; in a channel or query `/cmd` is a command, `//text` says `/text`, a lone `/` is text. The slash command table is one `enum SlashCommand` and one match; unknown commands go out as `UPPER rest`.
- `split(text, ctx)`: chunks of at most 510 minus `:hostmask CMD target :` minus CRLF, hostmask guessed at 60 when unknown, breaking on whitespace within the last 25 bytes when possible, never inside a UTF-8 sequence, formatting codes re-opened per chunk. Multi-line paste splits on `\n` first. `needs_flood_confirmation` (more than 4 lines or 4 by 510 bytes) returns an `Effect::Ui(ConfirmFlood)`; on yes the window dispatches the text again with the confirmed flag.
- `History`: Textual's semantics (100 entries, draft pushed on first Up, per-buffer or global per preference, drafts swapped on focus move) minus the dedup bug.
- `Completion::step(field, direction, candidates) -> Option<Edit>`: Textual's algorithm verbatim. Backward scan to whitespace or comma; `/` at line start completes commands, `@` nicks with the `@` restored, `#` channels, else nicks; prefix match on the lowered list, trimmed-nick aliases (`foo` matches `_foo`), repeated Tabs cycle with wrap, suffix `: ` at line start else a space. `Session::completion_source(buffer, now)` builds `Candidates` with members ordered by decayed weight then rank then nick, the current channel first, the services nicks and the network name appended, and an empty `nicks` list outside channels, which makes nick completion a no-op in queries and the console as in Textual.

**Config and secrets.** TOML, serde, two files. `config.toml` under `$XDG_CONFIG_HOME/textual-gtk/` holds intent: networks, servers, identity, channels, flags, style, input preferences. `state.toml` under `$XDG_STATE_HOME` holds runtime facts: selection, window geometry, sidebar width, `last_message_time` per network. Two files because they have two writers with different cadences (an explicit save after an edit or at quit versus a throttled 300 s save), and a runtime write must never be able to clobber a config edit. Defaults are stripped on save with `serde(default, skip_serializing_if)`, matching Textual's defaults-stripped dictionaries. `version = 1` at the top; `migrate(version, &mut Table)` is a chain of one function per version, run before deserialisation, and a newer version refuses to load rather than silently dropping keys. Saves are atomic (write to a temporary file, rename). GSettings loses because a list of networks each holding a list of servers and channels does not fit a flat schema without relocatable child schemas per UUID, the schema must be compiled and installed to run at all (hostile to `cargo run` and to the e2e harness), and a file is what users back up and sync.

Secrets: the `Secrets` trait with `get(kind, owner_uuid)` and `set`, implemented by `Keyring` over `oo7` (async, speaks Secret Service and falls back to its file keyring inside a sandbox, which is the Flatpak story) and by `MemorySecrets` for tests and e2e. Items carry attributes `application=textual-gtk`, `kind=server-password|nickserv|channel-key`, `id=<uuid>`, the same ownership Textual's `textual.server.<id>` labels express. Secrets are read once at startup into `Secret` fields on the config structs (`Debug` prints `[redacted]`, no `Serialize` impl, so one cannot leak into `config.toml` by construction). `Config::save` strips them and writes changed ones through the trait.

**Config edits reaching a live connection.** The dialog hands `World::replace_config` a whole `Config`. World diffs network ids: new ones get a `Slot`, removed ones get `Quit` then teardown, the rest get `Input::User(UserAction::Configure(network))`. Inside the session `Configure` diffs against `self.config`: a changed nick while online sends NICK; channels added with autojoin while online send JOIN and open the buffer; channels removed send PART and close; server list, TLS, SASL and identity changes are stored and print a console notice that they apply on the next connect. The reducer owns this policy, so it is a script test too.

**TLS.** `tokio-rustls` with roots from `rustls-native-certs` (the system store, which is what a Linux user expects and what the Flatpak runtime provides), SNI set to the host. `validate_certificate_chain = false` installs a verifier that accepts anything and prints a warning line. A rejected chain closes with `Closed::BadCertificate(details)` so the phase-3 trust prompt has what it needs; the session does not reconnect on it. Client certificates are an `Option<PathBuf>` field left for phase 3.

**Threading.** tokio owns one task per transport generation and knows nothing above lines. Everything else runs on the GTK main loop: `World` drains each transport's `events` receiver in a `glib::spawn_future_local`, exactly as `main.rs` does today. `Session`s sit inside one `Rc<RefCell<WorldInner>>`. `World::dispatch` holds the borrow only for `handle`, then applies effects with no borrow, so a GTK signal fired synchronously by an effect (a selection change from inserting a row, say) can dispatch again without a double borrow. That rule is documented on `dispatch` and is the one invariant the UI layer must keep. A single one-second `glib::timeout_add_seconds_local` sends `Input::Tick` to every session; all deadlines are instants inside `Phase`, so there is no per-session timer object to start, stop or leak.

### What the interface hides and exposes

The public surface of the core is `Session::new`, `Session::handle`, three read accessors, `Input`, `Effect`, and the state types. Behind it: the connection state machine, backoff, CAP 302, SASL, nick collisions, ISUPPORT, casemapping, membership tracking, NAMES/WHO merging, BATCH, destination routing, query auto-open, slash commands, splitting and echo policy. The UI never calls a protocol function; it dispatches an `Input` and matches an `Effect`. Exposed on purpose: the state types are plain `pub` structs read-only through `with_session`, because completion and the title need a snapshot and a copy-through-events design would duplicate the member table in the UI.

### What the design does not do

No flood control queue in phase 2 (`transport::Command::Send` is where it goes). No group selection or split panes. No highlight keyword lists beyond own nick, no ignore list, no notifications (`Effect::Print` already carries the `Line` with `highlight`, which is what a phase-3 `Notify` consumer needs). No ZNC playback request yet, though `server-time`, BATCH buffering and `last_message_time` persistence are in, so `play *` is one effect away. No logging; a phase-3 logger is another arm of `World::apply` on `Print`. No plugin hooks; the two interception points Textual has (before dispatch, before send) are the top of `handle` for `Transport{Line}` and `Typed`.

## Changes to existing code

- `irc/connection.rs` becomes `irc/transport.rs`: `Config` becomes `Endpoint { host, port, tls, connect_timeout }`; `respond()` and `Registration` are deleted (CAP, NICK, USER, 433, 001, PING and autojoin now come from the session as `Effect::Send`); `Event::Registered` is gone; `Disconnected(Disconnect)` becomes `Closed(Closed)` with the `BadCertificate` variant; `Command::Quit` becomes `Command::Close { grace }` since QUIT is just a `Send` now. `tests/connection.rs` loses its registration tests to `tests/session.rs` and gains a TLS test against an in-process rustls server with a self-signed certificate.
- `line.rs`: `Line::from_message(message, own_nick, casemap: CaseMapping)` so "mine" and STATUSMSG stripping fold correctly; `received_at` honours the `time` tag. `Destination` stays.
- `ui/chat_view.rs`: unchanged, except `new` takes a `related: Option<&WebView>`.
- `main.rs`: shrinks to loading config and secrets, building `World`, and `app.run`.

## Synthesis decision

Filled in by arena.

## Tradeoffs accepted

- We accept that every server line hops to the main thread before any reply (PONG included) in exchange for one owner of all protocol state and a reducer that runs in `cargo test`. The hop is a channel send; Textual did the same synchronously onto main.
- We accept a `HashMap` of members with sorting done by the reader (the GTK sorter, completion) in exchange for a session that holds no UI ordering policy. A QUIT costs one lookup per channel the user was in.
- We accept that the UI reads session state directly (snapshots for completion, title, member list rebuild) in exchange for not mirroring the member table into the UI layer through events.
- We accept a one-second global tick in exchange for no timer objects and deadlines that are plain `Instant`s in `Phase`, testable with `Script::advance`.
- We accept that `Effect` is one closed enum matched in one place, which grows an arm per new UI capability, in exchange for the effect application being the single file a reader opens to see what the UI does with the model.
- We accept TOML over GSettings, giving up `dconf-editor` and GSettings' change notifications, in exchange for a schema that fits nested lists, hand-editable files, and a harness that needs no installed schema.
- We accept dropping Textual's shared `IRCUser` with GC timers and relations in exchange for per-channel membership records; `away`, `account` and `hostmask` are stored per membership and updated in every channel the nick is in, which is a linear walk over channels, not users.

## Alternatives considered

- **Protocol state in the tokio actor, UI fed by events.** The actor would own nick, caps, channels and members, and the UI would need its own copies for completion, the title and the member list: two writers of one truth across threads, the thing the rubric and Textual's `@synchronized` zoo warn against. It hides the same complexity behind a bigger surface (every read becomes a message round trip or a mirror). Rejected.
- **Model as GObjects with property notifications and ListModels inside the model.** The sidebar and member list would bind directly to model objects, which is pleasant GTK. But the model then cannot be constructed or tested without GTK, GObject properties express scalar change well and list splices poorly, and every protocol handler would hold GObject references. Interface depth is low: the caller still has to know which property fires when. Rejected; the ListStores are projections instead.
- **Keep registration in the transport (grow phase 1's `respond()`).** It is already a small reducer with state; CAP 302 and SASL would triple it, and testing it means a socket pair per case. Moving it into `Session` costs nothing on the wire and makes the CAP table test possible. Rejected.
- **State-diffing instead of effects** (reducer returns new state, UI diffs old and new). Simple reducer signature, but O(members) per message and the UI has to rediscover what the reducer already knew (which nick changed). Rejected.
- **One `BufferId` space per network (small integers).** Cheaper than UUIDs but channel ids must survive restarts to key channel secrets and the saved selection, so they are UUIDs anyway; one type is simpler than two.

## Implementation reconciliation

Empty until implementation starts.

## Open questions and risks

- Should `autojoin_waits_for_nickserv` keep Textual's notice-scraping of NickServ replies in phase 2, or is "SASL succeeded, else join after 3 s" enough until the preference window lands? The sketch models the wait (`Autojoin::WaitingForNickServ`) but the token lists are a phase-3 detail.
- Is one shared web process (`related_view`) acceptable, given that a crash in one channel's page takes every view down (the existing crash recovery replays the last 1000 lines per view)? The alternative is a process per view, with the memory that implies at fifty channels.
- Does Michael want a Textual plist importer in phase 2 (`~/Library/Preferences` export from a Mac) or is a fresh config enough? The `migrate` chain can host an `import_textual(plist)` entry either way.
- Risk: `oo7` pulls zbus and an async runtime binding; the keyring read at startup must not block the main loop for long when the Secret Service prompts to unlock. Plan: read secrets on tokio before building the window, with a visible "unlocking keyring" state if it exceeds a second.
- Risk: `gtk::TreeListModel` with a `SingleSelection` loses the selection when a parent collapses; the sidebar must re-apply `focused` after `apply_buffer` the way Textual's `adjustSelection` does.
- Risk: rfc1459 default casemapping before 005 means a channel joined through `on_connect_commands` before 005 arrives would be keyed under rfc1459 and re-keyed on an `ascii` network; the re-key is covered by a test, but it is the one place two keys for one name can exist for a moment.

## Next implementation step

Create `src/irc/casemap.rs`, `isupport.rs`, `caps.rs` and `sasl.rs` with their table tests, then `src/session/mod.rs` with `Phase`, `Input`, `Effect` and a `handle` that passes the CAP 302 and reconnect scripts in `tests/session.rs` through `tests/support/script.rs`, before any widget is touched.

## Principles that shaped this

- **model-the-domain**: `Phase` as one enum with deadlines in its variants replaced Textual's `isConnecting`/`isConnected`/`isLoggedIn`/`isQuitting`/`reconnectEnabled` booleans and six timers; `Caps` and `SaslState` as step functions replaced flag bits hidden in a pending array.
- **foundational-thinking**: identities, keys and `Phase` were fixed before any handler; the access patterns (message to channel, nick in channel, channels of nick, ordered members) decided `HashMap`s over sorted vectors and a reader-side sort.
- **separate-before-serializing-shared-state**: protocol state has one owner on one thread; unread counters are owned by the sidebar rows, not mirrored in the session; `config.toml` and `state.toml` are two files because they have two writers.
- **type-system-discipline**: `NickKey` and `ChanKey` have private constructors; `Secret` cannot serialise; `ConnectionId` makes stale transport events a typed non-event; `Desired` is not a boolean.
- **boundary-discipline**: `Session::handle` is pure apart from `now`; parsing config, secrets, and lines happens at the edges; `World::apply` is the only place GTK and model meet.
