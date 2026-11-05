# Arena brief: textual-gtk phase-2 client model

## Goal (whose and why)

Michael is rebuilding the macOS IRC client Textual for Linux in Rust + GTK4 + libadwaita + WebKitGTK 6.0. Phase 1 is done: one hard-coded server and channel render live in a WebKit view through Textual's real templates. Phase 2 turns it into a real client. Read `/home/mae/projects/textual-gtk/PLAN.md` (phases, mapping table, Status section) and `DECISIONS.md` first.

Phase 2 scope (from PLAN.md): multiple servers; a server/channel list (sidebar) and a member list; input with history and nick completion; TLS, SASL, credentials in libsecret; persisted config.

## The artifact you produce

A design package for the phase-2 **client model and its wiring**: the Rust module map, the core types and function signatures (bodies `todo!()`), and how data flows from a socket line to the sidebar, member list, chat views and title, and from the input field back to the socket. Write it as one Markdown file `design.md` plus optional `sketch.rs` in your output directory. Follow the architect runner prompt you were given (usage first, data structures first, rationale per the template).

## Existing code you build on (read it)

`/home/mae/projects/textual-gtk/src/`:
- `irc/message.rs`: IRCv3 `Message` parse / `to_wire`. Keep.
- `irc/connection.rs`: one tokio task per connection; `spawn(&runtime, Config) -> Handle { commands, events }`; events `Connected / Registered / Message / Refused / Disconnected(Disconnect)`. It owns registration (NICK/USER, 433 retry, PING, autojoin) today. You may move responsibilities in or out of it (CAP/SASL, TLS, reconnect), but say why.
- `line.rs`: `Line::from_message(&Message, own_nick) -> Option<(Destination, Line)>` and `Line::render(&Style)`.
- `style/`: style loading, template engine, IRC formatting renderer.
- `ui/chat_view.rs`: one WebKit view per channel; keeps its last 1000 lines and replays them after reloads.
- `main.rs`: phase-1 glue (one server, one channel).
- Tests: `tests/*.rs`, `scripts/e2e.py` (Xvfb end-to-end against a local Ergo).

## Grounding: how Textual does it (read these)

- `/home/mae/projects/textual-gtk/docs/research/phase2-irc-model.md`: IRCWorld/IRCClient/IRCChannel/IRCUser ownership, connection state machine, CAP/SASL, ISUPPORT, membership tracking, reconnect, ZNC, threading, and a list of Textual bugs we should not copy (no CASEMAPPING, broken CAP LS 302 multiline, no reconnect backoff, NAMES early-return bug, ...).
- `/home/mae/projects/textual-gtk/docs/research/phase2-window-and-input.md`: main window composition, selection (group + focused item), unread/highlight counters, member list sort, input history, the exact nick-completion algorithm, typed-line handling, shortcuts.
- `/home/mae/projects/textual-gtk/docs/research/webview.md` and `templates.md`: the chat view and template contract (already implemented).
- Original source (read-only, for anything the notes leave open): `~/workspace/Textual-4f09312`.

Config and print-path facts (from a third research pass whose notes were not saved):
- Per server ("client"): uniqueIdentifier (UUID), connectionName, a list of server addresses (address, port, TLS flag, own UUID), nickname, alternateNicknames, username, realName, nicknamePassword (also the SASL PLAIN password; SASL authcid is the username), autoConnect, autoReconnect, autojoinWaitsForNickServ, onConnectCommands, encodings, proxy, flood control, client certificate, validateServerCertificateChain, sidebarItemExpanded, lastMessageServerTime (for ZNC playback), channelList, highlightList, ignoreList.
- Per channel: uniqueIdentifier, channelName, type (channel / private message / utility), autoJoin, JOIN key, defaultModes/defaultTopic, notification overrides.
- Textual stores everything as defaults-stripped dictionaries in user defaults, secrets in the keychain keyed by owner UUID (`textual.server.<serverId>`, `textual.nickserv.<clientId>`, `textual.cjoinkey.<channelId>`, ...). Saves are explicit (after edits, at quit) plus throttled (300 s) for runtime state.
- Print path: every printed line goes through one funnel that picks the destination view (channel, query auto-created for incoming PMs, server console for server notices/numerics), collects highlight keywords, renders, then bumps unread/highlight counters and posts notifications only after render. Public messages to channels we are not in are dropped. With echo-message, outgoing text is not printed locally.

## Decisions already made (do not reopen)

- Rust, gtk4-rs 0.11 (GTK 4.14 on the dev box), libadwaita 0.9 (1.5), webkit6, tokio. Changes land on main after an independent review.
- libadwaita look by default, plus a later "Textual for Mac" appearance setting that swaps in an app stylesheet. Widgets therefore need stable CSS class names.
- Keep Textual's structure and behaviour; GNOME idiom for chrome.
- Config format: open (GSettings vs TOML vs other). Decide and justify. Secrets go to libsecret (crates `oo7` or `secret-service`); decide.
- TLS: rustls (with the platform verifier or webpki roots; decide), client certificates later.

## What the design must answer

1. Who owns protocol/session state (nick, caps, ISUPPORT, channels, members, topics) and on which thread. The GTK main loop is single-threaded; tokio runs the sockets. One owner per piece of state.
2. The connection lifecycle as an explicit state machine (connecting, registering with CAP/SASL, connected, disconnected with reason, reconnect with backoff, quitting), and how config edits reach a live connection.
3. Message → state update → UI update. Is the reducer pure and testable without GTK? How does the UI learn what changed (events, diffs, observers, GObject properties/ListModels)?
4. Casemapping-correct identity for nicks and channels (CASEMAPPING ascii / rfc1459 / rfc1459-strict).
5. The sidebar and member list models (GTK ListModel / TreeListModel?), selection, unread/highlight counts, and where the chat views live (one per buffer, created lazily?).
6. Input: history, nick completion, slash commands, splitting long lines to the byte budget, multi-line paste.
7. Config + secrets persistence and migration story; when it saves.
8. How it is tested headless (protocol fixtures, a fake server, reducer tests) and how the e2e harness grows.

Keep scope to phase 2. Note phase-3/4 hooks (notifications, logging/history search, plugins) only where the shape must leave room.
