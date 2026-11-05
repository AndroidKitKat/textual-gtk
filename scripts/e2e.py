#!/usr/bin/env python3
"""End-to-end check for phase 1.

Starts the real app under Xvfb against a local IRC server, has a second
client say something in the app's channel, and passes only if the app's
page shows that line (read from the page's DOM through the probe hook).

    scripts/e2e.py [--server 127.0.0.1:6667] [--out DIR]

Needs: a built target/debug/textual-gtk, xvfb-run, and an IRC server (the
local Ergo container: docker start textual-ergo).
"""

import argparse
import json
import os
import secrets
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def fail(message):
    print(f"FAIL: {message}")
    sys.exit(1)


class Client:
    def __init__(self, host, port, nick):
        self.sock = socket.create_connection((host, port), timeout=10)
        self.buf = b""
        self.send(f"NICK {nick}")
        self.send(f"USER {nick} 0 * :{nick}")
        self.wait_for(lambda line: " 001 " in line)

    def send(self, line):
        self.sock.sendall(line.encode() + b"\r\n")

    def wait_for(self, predicate, timeout=15):
        deadline = time.time() + timeout
        while time.time() < deadline:
            while b"\r\n" in self.buf:
                raw, self.buf = self.buf.split(b"\r\n", 1)
                line = raw.decode(errors="replace")
                if line.startswith("PING"):
                    self.send("PONG" + line[4:])
                elif predicate(line):
                    return line
            self.sock.settimeout(max(0.1, deadline - time.time()))
            try:
                chunk = self.sock.recv(4096)
            except socket.timeout:
                break
            if not chunk:
                break
            self.buf += chunk
        return None


def read_probe(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--server", default="127.0.0.1:6667")
    parser.add_argument("--out", default=str(ROOT / "target/e2e"))
    args = parser.parse_args()
    host, port = args.server.rsplit(":", 1)
    port = int(port)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    probe = out / "probe"
    for suffix in (".json", ".png"):
        probe.with_suffix(suffix).unlink(missing_ok=True)

    token = secrets.token_hex(4)
    channel = f"#e2e-{token}"
    app_nick = f"app{token}"
    binary = ROOT / "target/debug/textual-gtk"
    if not binary.exists():
        fail(f"{binary} is missing; run cargo build")

    env = dict(os.environ, TEXTUAL_GTK_PROBE=str(probe), GSK_RENDERER="cairo")
    # A private D-Bus session keeps GApplication from handing activation to
    # an instance that is already running (a leftover run, or Michael's).
    app = subprocess.Popen(
        ["dbus-run-session", "--", "xvfb-run", "-a", "-s", "-screen 0 1100x700x24",
         str(binary), "--server", args.server, "--nick", app_nick, "--channel", channel],
        env=env, stdout=open(out / "app.log", "w"), stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    try:
        talker = Client(host, port, f"talk{token}")
        talker.send(f"JOIN {channel}")
        talker.send(f"TOPIC {channel} :topic {token}")
        joined = talker.wait_for(lambda l: f"JOIN {channel}" in l and l.startswith(f":{app_nick}!"), 30)
        if joined is None:
            talker.send(f"NAMES {channel}")
            names = talker.wait_for(lambda l: " 353 " in l, 5)
            if names is None or app_nick not in names:
                fail(f"the app never joined {channel}; see {out / 'app.log'}")

        text = f"hello from e2e {token} <script>x</script>"
        talker.send(f"PRIVMSG {channel} :\x02bold\x02 {text}")

        deadline = time.time() + 20
        state = None
        while time.time() < deadline:
            state = read_probe(probe.with_suffix(".json"))
            if state and token in state.get("last", ""):
                break
            if app.poll() is not None:
                fail(f"the app exited with {app.returncode}; see {out / 'app.log'}")
            time.sleep(0.2)
        else:
            fail(f"the line never showed up; last probe: {state}")

        time.sleep(3)
        state = read_probe(probe.with_suffix(".json"))
        checks = {
            "message is the last line": token in state["last"],
            "sender is shown": f"talk{token}" in state["last"],
            "html in the message stays text": "<script>x</script>" in state["last"],
            "earlier lines are there too": state["lines"] >= 2,
            "the style faded the page in": float(state["bodyOpacity"]) > 0.99,
            "the topic bar shows the topic": state["topic"] == f"topic {token}",
            "snapshot written": probe.with_suffix(".png").exists(),
        }
        for name, ok in checks.items():
            print(f"{'ok  ' if ok else 'FAIL'} {name}")
        print(json.dumps(state, indent=2))
        print(f"screenshot: {probe.with_suffix('.png')}")
        talker.send("QUIT :done")
        if not all(checks.values()):
            sys.exit(1)
        print("PASS")
    finally:
        os.killpg(app.pid, signal.SIGTERM)
        try:
            app.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(app.pid, signal.SIGKILL)


if __name__ == "__main__":
    main()
