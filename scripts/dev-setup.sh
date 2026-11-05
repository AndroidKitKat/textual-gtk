#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

sudo apt-get install -y libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev \
  libsecret-1-dev pkg-config xvfb

if [[ -r /proc/sys/kernel/apparmor_restrict_unprivileged_userns ]] &&
   [[ "$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns)" == 1 ]]; then
  sudo install -m 644 packaging/apparmor/textual-gtk-dev /etc/apparmor.d/textual-gtk-dev
  sudo apparmor_parser -r /etc/apparmor.d/textual-gtk-dev
fi
