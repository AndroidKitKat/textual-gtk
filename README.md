# Textual GTK

A Linux rebuild of the macOS IRC client [Textual](https://github.com/Codeux-Software/Textual) in Rust with GTK 4, libadwaita and WebKitGTK 6.0. Work in progress; see `PLAN.md`.

## Build and run

```sh
scripts/dev-setup.sh        # system libraries, plus an AppArmor profile on Ubuntu 24.04+
cargo build
target/debug/textual-gtk --server 127.0.0.1:6667 --nick me --channel '#textual'
```

Options: `--server host:port`, `--nick`, `--channel`, `--style` (a folder name under `resources/styling/Bundled Styles`, default `Simplified`).

## Checks

```sh
scripts/check.sh            # fmt, clippy, unit and integration tests
scripts/test-server.sh      # local Ergo on 127.0.0.1:6667/6697, limits lifted for tests
cargo test --test connection -- --ignored   # two clients through the real server
python3 scripts/e2e.py      # the app under Xvfb shows a live message
```

## License

The styles, templates and JavaScript in `resources/styling` come from Textual and keep their BSD licence; see `resources/styling/LICENSE.md`.
