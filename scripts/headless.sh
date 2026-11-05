#!/usr/bin/env bash
# Runs a command on a private X server, never on the desktop: DISPLAY and
# WAYLAND_DISPLAY are dropped first, so a GUI process that ignores xvfb-run's
# DISPLAY still cannot reach the real screen.
#
#   scripts/headless.sh target/debug/textual-gtk --network ...
set -euo pipefail
exec env -u DISPLAY -u WAYLAND_DISPLAY xvfb-run -a -s "-screen 0 1280x800x24" "$@"
