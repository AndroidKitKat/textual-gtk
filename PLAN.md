# textual-gtk: plan

Goal: rebuild the macOS IRC client Textual for Linux in Rust with GTK4 and libadwaita. Keep its design and behaviour as close as practical, written in GNOME's idiom rather than copying macOS widgets.

This is a rewrite that follows the original, not a code port. No Objective-C or Swift is reused. The chat view's HTML, CSS, JS and Mustache templates can be carried over almost as they are.

Written 2026-11-05 from the conversation in which Michael proposed the project.

## Source of truth

- **Base: `~/workspace/Textual-4f09312`.** This is Michael Morris's (Codeux) last commit, `4f093128c` "I can't spell", 2026-07-02. Use this tree, not the newer one.
- `~/workspace/Textual` is the newer Textual 8 work in progress (HEAD `860a4bb8d`, 2026-10-20). It is 249 commits past 4f09312, almost all by Daniel (bashgeek), the new maintainer. Use it only for reference.
- Read-only archive: `~/archive/textual-codeux-final-4f09312/`. It has git bundles of every repo and submodule (Auto-Hyperlinks, Cocoa-Extensions, Encryption-Kit, Static-Libraries, Textual-Swift-Plugin) and a SHA256SUMS file.
- License: BSD 3-Clause. Keep the copyright and license notices for anything derived from it, especially styles and templates.

## What 4f09312 contains

- About 128k lines of `.m`, 139k of headers, 3.6k of Swift and 42 XIBs.
- `Sources/App/Classes`: IRC is about 25k lines, Views 22k, Dialogs 15k, Preferences 7k, Controllers 5k.
  - The IRC core is mostly Foundation-only, so it translates straight across.
- Sockets: GCDAsyncSocket (patched), in `Sources/Shared/Library/External Libraries/Sockets`.
- XPC services:
  - IRC Remote Connection Manager (Swift `IRCConnection.swift`)
  - Historic Log File Manager (Core Data)
  - Inline Content Loader
- Chat view: code paths for both the legacy WebKit1 `WebView` and `WKWebView`. Port only the WKWebView one.
  - It uses the bundled styles, the GRMustache templates and the JS API (`TVCLogScriptEventSink`).
- Frameworks: Auto Hyperlinks, Cocoa Extensions, Encryption Kit (OTR), Static Libraries.
- Plugins (9): Blowfish Encryption, Caffeine, Chat Filter, Sample Code, Smiley Converter, System Profiler, User Insights, Wiki Link Parser, ZNC Additions.

## Mapping

| Textual (macOS) | Rust / Linux |
|---|---|
| WKWebView + JS bridge | `webkit6` (WebKitGTK 6.0), `UserContentManager` script message handlers |
| GRMustache | `ramhorns` or `mustache`. First check which GRMustache-only features the templates use |
| IRC core | Rewrite on `tokio`. Parser, ISUPPORT, CAP/SASL, ZNC playback |
| GCDAsyncSocket / TLS | `tokio` + `rustls`; proxies via `tokio-socks` |
| XPC services | Async tasks inside the app (subprocesses only if isolation turns out to matter) |
| Keychain | libsecret (`oo7` or `secret-service`) |
| NSUserDefaults (TPCPreferences) | GSettings or TOML config. Decide in phase 2 |
| Core Data history log | SQLite (`rusqlite`) |
| Main window: server list, member list, input, split view | `gtk4` + `libadwaita`: `AdwOverlaySplitView`, `ListView`, `TextView` |
| Preferences window | `AdwPreferencesDialog` |
| Notifications | `GNotification` |
| Dock badges | No real equivalent. Maybe an unread count in the title or a background portal |
| Caffeine plugin | logind / portal inhibit |
| Encryption Kit (OTR) | **Open decision:** FFI to `libotr`, or drop OTR |
| Objective-C plugins | New plugin API: Rust, WASM or external scripts. **Open decision**, deferred to phase 4 |
| AppleScript / user scripts | Plain executables |
| Sparkle, License Manager | Drop. Distribute as Flatpak later |

## Phases

1. **Proof of concept.** Connect to one server, join one channel, and render live messages with a bundled style from 4f09312 in WebKitGTK through the original templates and JS. This tests the riskiest assumption: that the style and template pipeline carries over.
2. **Core client.**
   - Multiple servers, a server/channel list and a member list.
   - Input with history and nick completion.
   - TLS, SASL, and credentials in libsecret.
   - Persisted config.
3. **Parity.** Preferences, highlights, notifications, logging and history search, inline media, DCC, and the remaining dialogs (server properties, channel properties, moderation and so on).
4. **Extensibility.** Plugin API, user scripts, custom user styles, and porting the useful plugins.

## Status

- **Phase 1: done (2026-11-05).** `scripts/e2e.py` runs the app under Xvfb against the local Ergo. A second client's message shows in the app's WebKitGTK page through Simplified's real templates, CSS and JS, with the topic bar set. Pieces:
  - `src/irc`: IRCv3 line parser (ircdocs vectors) and a tokio connection task (registration, PING, autojoin, quit).
  - `src/style`: style loading, the template engine, and IRC formatting to HTML.
  - `src/line.rs`: messages to Textual log lines (wording, nick colours, timestamps).
  - `src/ui/chat_view.rs`: the WebKit view and Textual's JS handshake.
- **Deferred from phase 1 to phase 2:** outgoing line length limits and splitting, PING idle timeout, registration failures other than 433, NAMES/member tracking (QUIT and NICK lines currently show in the one channel regardless), links, channel and nick tagging in bodies. Also: show a visible notice (not only stderr) when the view gives up after repeated web-process crashes, and echo a sent line only after the connection accepts it. From the unit 6 re-review: a query renamed by NICK keeps its old nick in its page file, so a reload shows the old name to styles that read `channelName` (regenerate the page on rename); kick-rejoin omits Textual's `errorOnLastJoinAttempt` condition (needs per-channel join-error state); `--help` after a flag that takes a value is read as that value.
- **Phase 2, units 1-6 done (2026-11-05):** `docs/design/phase2.md` is the contract. `crates/core` holds the protocol, lifecycle, membership, print funnel, scrollback, counters, autojoin and the pure `Workspace`; the app runs on it through one loop (`src/app.rs`) with the sidebar, member list, chat stack and title in `src/ui`. `scripts/e2e.py` drives two networks on the local Ergo. The phase-1 connection code is gone. - **Phase 2, unit 7 done (2026-11-05):** a multi-line input with Textual's commands, wire splitting, history and Tab completion, checked against ports of Textual's own algorithms. Its fix round (5a1d271..6b235e9) matched the review's Textual differences and passed re-review; the re-review's three small findings are in the backlog below.

## What's next (written 2026-11-05, before a break)

Work paused after unit 7 at Michael's request. Pick up here.

1. **Unit 8: config and secrets** (last unit of phase 2; the design is `docs/design/phase2.md`, "Config and secrets", plus its Implementation reconciliation entries).
   - `config.toml` (networks, servers, identity, auth, channels) and `state.toml` (selection, expanded rows, window size, `last_server_time`), defaults stripped, versioned with migrations, atomic writes; replace the `--network` flags (keep them only as overrides, or drop them).
   - Secrets in libsecret through `oo7`: server password, NickServ/SASL password, channel keys; nothing in the TOML. `SecretsLoaded` gates connecting so a locked keyring never blocks the window.
   - Widen or map `BufferId(u64)` so configured channels keep their config UUID; validate config so `add_network` cannot hit its duplicate-id assert; `UiAction::Close` on a configured channel must remove it from the config.
   - Preferences that are flags today move into the config with Textual's defaults: `--rejoin-on-kick`, `--input-history-per-buffer`, `--command-return-sends-action`, `--control-enter-sends-message`, `focus_on_message_command`, completion suffix.
   - A minimal Add Server dialog (name, address, port, TLS, nick, auth, password).
   - e2e: restart restores servers, selection and expansion; SASL PLAIN against an Ergo account; TLS with `verify = true` ending in BadCertificate on Ergo's self-signed port.
2. **Backlog found by reviews, not yet fixed** (low unless marked):
   - Medium-low (unit 7 re-review): the nick ladder only advances on a 432/433/437 that names its exact nick (rfc1459 fold); a server that truncates or normalises the nick in its reply leaves registration stuck until the 240 s watchdog. Textual advances on any refusal before 001; fix by tracking the nicks the user typed instead.
   - Low (unit 7 re-review): `Debug for WireLine` misses lower-case or space-padded `pass`/`authenticate` lines sent with `/raw` (latent: nothing prints them today); make the check case-insensitive and skip runs of spaces.
   - Low (unit 7 re-review): `/ctcp nick VERSION` sends `\x01VERSION\x01`; Textual sends `VERSION ` with a trailing space. Under the copy-Textual rule, match it. Textual's `/ctcp` also takes its target from the selected channel in one case.
   - Medium: a 005 PREFIX change after a channel is joined re-ranks members without a MemberDiff, so the member list drifts.
   - CTCP: only ACTION is handled; Textual answers VERSION, PING, TIME and so on, and prints CTCP replies. Non-ACTION CTCP is dropped today.
   - A query renamed by NICK keeps its old nick in its page file (regenerate the page on rename).
   - Kick-rejoin omits Textual's `errorOnLastJoinAttempt` condition.
   - Show a visible notice (not only stderr) when a chat view gives up after repeated web-process crashes.
   - Hidden WHO with no 315 swallows the user's own later WHO replies for that channel; a 353 collected before a QUIT can resurrect the quitter at 366; pre-001 PRIVMSG/NOTICE from users are dropped.
   - `/names` replies are hidden (Textual shows them in a utility window); Textual commands not yet mapped still go out raw (`server`, cycle/hop/rejoin, umode, who, list).
   - The link detector is a stand-in (scheme:// and www. to whitespace); real link parsing comes with linkification.
   - Transport: a write error can lose a server's final ERROR; a peer that stops reading stalls PONG until the idle close.
   - `--help` after a flag that takes a value is read as that value.
3. **Phase 3** (parity): preferences window; the "Textual for Mac" appearance stylesheet (needs screenshots of Textual on Michael's Mac); highlight keywords and ignores; notifications; logging and history search; links, inline media; DCC; the remaining dialogs (server and channel properties, moderation); menus for connect/disconnect and the rest of Textual's menu-only actions; OTR decision.
4. **Phase 4**: plugin API, user scripts, user styles, porting the useful plugins.

State when paused:
- `main` on GitHub has everything reviewed; nothing is left uncommitted that matters (the `.audit/unit*` files are local working notes).
- A release binary built from 2e34a96 is at `target/release/textual-gtk` for Michael to try (run it with `TEXTUAL_GTK_RESOURCES=~/projects/textual-gtk/resources/styling`; README lists the `--network` keys). It is replaced by any later `cargo build --release`.
- The local Ergo runs in docker as `textual-ergo` (`scripts/test-server.sh` restarts it).
- Every GUI run by an agent goes through `scripts/headless.sh` or `scripts/e2e.py`, never the desktop.

## Open decisions (Michael)

- **OTR:** keep it (via libotr) or drop it? Due in phase 3.
- **Leaving comment:** Textual's default is "Textual IRC Client: www.textualapp.com"; ours is "Textual GTK" so we do not advertise another app. Michael can change it.
- Answered: copy Textual's behaviour and defaults as closely as possible, with libadwaita plus a "Textual for Mac" stylesheet setting (see `DECISIONS.md`).

If Michael hasn't answered, pick a default, log it in `DECISIONS.md`, and keep going.

## Environment (koya)

- **Ubuntu 24.04, 12 cores, 14 GB RAM.** Passwordless `sudo` works.
- **Rust:** in `~/.cargo/bin`, which may not be on PATH in non-login shells.
- **System libraries:** not installed yet. Run:
  `sudo apt install libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev libsecret-1-dev pkg-config`
- **Debugging:** `gdb` is installed. For a stack dump, use `gdb -p PID -batch -ex "thread apply all bt"`. `strace` and `perf` are also there. Install elfutils (`eu-stack`) or valgrind if needed.
- **GUI testing:**
  - The X display is `:0` and is reachable. `xdotool` and `gnome-screenshot -f out.png` are installed. Michael can watch at http://koya:6080 (noVNC).
  - Use Xvfb for automated runs, so they don't take over the real desktop.
- **IRC testing:** run a local server (e.g. Ergo in Docker; Docker is installed). Don't test half-built code on Libera.
- **The pstack plugin is installed.** Route multi-file and design work through `pstack:poteto-mode`.
- **Reaching Michael:** `~/bin/notify-claudia "<summary>" textual-gtk`.
- **Remote:** `git@github.com:AndroidKitKat/textual-gtk.git` (`origin`). koya's `~/.ssh/id_ed25519` has push access. Push after each commit.

## Working rules

- Commit often and keep this file and `DECISIONS.md` current. Progress has to survive context compaction and session restarts.
- Read the original source to match behaviour. There is no Mac on hand to compare against side by side.
- Tell Michael when a milestone is reached, when something is blocked, or when a decision is needed.
