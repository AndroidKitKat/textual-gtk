# Textual GTK

A Linux rebuild of the macOS IRC client [Textual](https://github.com/Codeux-Software/Textual) in Rust with GTK 4, libadwaita and WebKitGTK 6.0. Work in progress; see `PLAN.md`.

## Build and run

```sh
scripts/dev-setup.sh        # system libraries, plus an AppArmor profile on Ubuntu 24.04+
cargo build
target/debug/textual-gtk \
  --network 'name=Libera,host=irc.libera.chat,tls=on,nick=me,join=#textual' \
  --network 'name=Local,host=127.0.0.1,port=6667,nick=me,join=#a #b:key'
```

Each `--network` is a comma-separated list of `key=value` pairs and adds one network to the sidebar. `name`, `host` and `nick` are required; names must differ, ignoring case. Optional: `port` (1 to 65535; 6667, or 6697 when `tls=on`), `tls=off|on` (off), `verify=true|false` (true; whether to check the server's certificate chain, with `tls=on`), `user` (the nick), `realname` (`Textual GTK`), `join` (space-separated channels to join once online; `#chan:key` for a keyed channel). An unknown key, or a key given twice, is an error. To put a comma in a value, write `\,`; write `\\` for a backslash, as in `realname=Doe\, Jane`.

`--rejoin-on-kick true|false` (false) joins a channel again three seconds after a kick, like Textual's "Automatically rejoin a channel when kicked" preference (`RejoinChannelOnLocalKick`), which applies to every network. `--style name` picks a folder under `resources/styling/Bundled Styles` (default `Simplified`). `--help` prints the usage. The saved configuration replaces these flags in a later unit.

## Checks

```sh
scripts/check.sh            # fmt, clippy, unit and integration tests
scripts/test-server.sh      # local Ergo on 127.0.0.1:6667/6697, limits lifted for tests
cargo test --test transport -- --ignored    # the model and transport against the real server
python3 scripts/e2e.py      # the app under Xvfb on two networks and a fake server: sidebar, members, badges, reload, quit
```

## License

The styles, templates and JavaScript in `resources/styling` come from Textual and keep their BSD licence; see `resources/styling/LICENSE.md`.
