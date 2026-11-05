#!/usr/bin/env bash
# Runs a command away from the desktop session: a private X server, a private
# D-Bus session bus, the X11 backend, and in-memory GSettings, with the
# desktop's DISPLAY, WAYLAND_DISPLAY and bus address removed first. So a GUI
# process can neither draw on the real screen nor reach the real session
# (GApplication activation, settings, portals, notifications).
#
#   scripts/headless.sh target/debug/textual-gtk --network ...
set -euo pipefail
exec env -u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS \
  GDK_BACKEND=x11 GSETTINGS_BACKEND=memory \
  dbus-run-session -- xvfb-run -a -s "-screen 0 1280x800x24" "$@"
