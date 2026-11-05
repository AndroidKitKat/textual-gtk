#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# The core crate must stay free of GUI and async crates so the model is
# testable without a window (docs/design/phase2.md, "Crates"). Build
# dependencies and optional features count too.
tree=$(cargo tree -p textual-core -e normal,build --all-features --prefix none)
forbidden=$(tail -n +2 <<<"$tree" |
  grep -Eo '^(gtk4|libadwaita|glib|gio|gdk4|webkit6|javascriptcore6|tokio) ' | sort -u || true)
if [[ -n "$forbidden" ]]; then
  echo "textual-core must not depend on: $forbidden" >&2
  exit 1
fi
