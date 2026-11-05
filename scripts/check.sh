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

# Setting a widget's class list replaces the classes GTK and libadwaita
# put there (a window's `background`, a box's `horizontal`), which left the
# header bar transparent once. Add classes one by one instead.
# Perl reads each file whole, so a call whose arguments start on the next
# line (rustfmt's shape for long lists) is caught too.
if replaced=$(find src -name '*.rs' -print0 | xargs -0 perl -0777 -ne \
    'print "$ARGV\n" if /\.css_classes\s*\(\s*[^)\s]|set_css_classes|"css-classes"/'); [[ -n "$replaced" ]]; then
  echo "$replaced" >&2
  echo "use add_css_class, or Bound::classes for a row's classes, instead of replacing the class list" >&2
  exit 1
fi
