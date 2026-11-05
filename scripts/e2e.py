#!/usr/bin/env python3
"""End-to-end check for phase 1.

Starts the real app under Xvfb against a local IRC server, has a second
client set the topic and say something in the app's channel, and passes
only if the app's page shows it (read from the page's DOM through the
TEXTUAL_GTK_PROBE hook). Then reloads the page and kills its web process,
checking both times that the lines come back, clears the topic, and closes
the window, checking the server sees the app quit.

    scripts/e2e.py [--server 127.0.0.1:6667] [--out DIR]

Needs: a built target/debug/textual-gtk, xvfb-run, dbus-run-session, an
IRC server (the local Ergo: docker start textual-ergo), and on Ubuntu
24.04+ the AppArmor profile from scripts/dev-setup.sh, without which
WebKit's sandbox cannot start.
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
        if self.wait_for(lambda line: " 001 " in line) is None:
            fail(f"{nick} could not register with the server")

    def send(self, line):
        self.sock.sendall(line.encode() + b"\r\n")

    def wait_for(self, predicate, timeout=15, alive=lambda: True):
        deadline = time.time() + timeout
        while time.time() < deadline and alive():
            while b"\r\n" in self.buf:
                raw, self.buf = self.buf.split(b"\r\n", 1)
                line = raw.decode(errors="replace")
                if line.startswith("PING"):
                    self.send("PONG" + line[4:])
                elif predicate(line):
                    return line
            self.sock.settimeout(min(0.5, max(0.1, deadline - time.time())))
            try:
                chunk = self.sock.recv(4096)
            except socket.timeout:
                continue
            if not chunk:
                break
            self.buf += chunk
        return None


class Probe:
    def __init__(self, prefix, app, log):
        self.prefix = prefix
        self.app = app
        self.log = log

    def state(self):
        try:
            return json.loads(self.prefix.with_suffix(".json").read_text())
        except (OSError, ValueError):
            return None

    def trigger(self, name):
        self.prefix.with_suffix(f".{name}").touch()

    def wait_until(self, predicate, what, timeout=20):
        deadline = time.time() + timeout
        state = None
        while time.time() < deadline:
            if self.app.poll() is not None:
                fail(f"the app exited with {self.app.returncode} while waiting for {what}; see {self.log}")
            state = self.state()
            if state and predicate(state):
                return state
            time.sleep(0.2)
        fail(f"{what} never happened; last probe: {state}")


def descendants(root):
    """Pids under `root`, so only this run's processes are touched."""
    table = subprocess.run(["ps", "-e", "-o", "pid=,ppid=,comm="], capture_output=True, text=True).stdout
    children = {}
    for row in table.splitlines():
        pid, ppid, comm = row.split(None, 2)
        children.setdefault(int(ppid), []).append((int(pid), comm.strip()))
    found, stack = [], [root]
    while stack:
        for pid, comm in children.get(stack.pop(), []):
            found.append((pid, comm))
            stack.append(pid)
    return found


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--server", default="127.0.0.1:6667")
    parser.add_argument("--out", default=str(ROOT / "target/e2e"))
    args = parser.parse_args()
    host, port = args.server.rsplit(":", 1)
    port = int(port)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    prefix = out / "probe"
    for suffix in (".json", ".png", ".reload", ".close"):
        prefix.with_suffix(suffix).unlink(missing_ok=True)
    log = out / "app.log"

    token = secrets.token_hex(4)
    channel = f"#e2e-{token}"
    app_nick = f"app{token}"
    talker_nick = f"talk{token}"
    binary = ROOT / "target/debug/textual-gtk"
    if not binary.exists():
        fail(f"{binary} is missing; run cargo build")

    # A private D-Bus session keeps GApplication from handing activation to
    # an instance that is already running (a leftover run, or Michael's).
    env = dict(os.environ, TEXTUAL_GTK_PROBE=str(prefix), GSK_RENDERER="cairo")
    app = subprocess.Popen(
        ["dbus-run-session", "--", "xvfb-run", "-a", "-s", "-screen 0 1100x700x24",
         str(binary), "--server", args.server, "--nick", app_nick, "--channel", channel],
        env=env, stdout=open(log, "w"), stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    probe = Probe(prefix, app, log)
    checks = {}
    try:
        talker = Client(host, port, talker_nick)
        talker.send(f"JOIN {channel}")
        talker.send(f"TOPIC {channel} :topic \x02{token}\x02")
        joined = talker.wait_for(
            lambda l: l.startswith(f":{app_nick}!") and f"JOIN {channel}" in l,
            30, alive=lambda: app.poll() is None,
        )
        if joined is None:
            hint = " (is the AppArmor profile from scripts/dev-setup.sh loaded?)" if "bwrap" in log.read_text() else ""
            fail(f"the app never joined {channel}{hint}; see {log}")

        text = f"hello from e2e {token} <script>x</script>"
        talker.send(f"PRIVMSG {channel} :\x02bold\x02 {text}")
        probe.wait_until(lambda s: text in s.get("last", ""), "the message to show up")
        time.sleep(2)
        state = probe.state()
        checks["the message is the last line"] = text in state["last"]
        checks["the sender is shown"] = f"<{talker_nick}>" in state["last"]
        checks["html in the message stays text"] = "<script" not in state["lastHtml"]
        checks["formatting codes render as markup"] = "<b>bold</b>" in state["lastHtml"]
        checks["no control characters reach the text"] = not any(ord(c) < 0x20 for c in state["last"])
        checks["the join line came first"] = state["lines"] >= 2
        checks["the style faded the page in"] = float(state["bodyOpacity"]) > 0.99
        checks["the topic bar shows the topic"] = state["topic"].replace("\xa0", " ") == f"topic {token}"
        before = state

        probe.trigger("reload")
        after = probe.wait_until(
            lambda s: s["loadedAt"] != before["loadedAt"]
            and s["lines"] == before["lines"]
            and float(s["bodyOpacity"]) > 0.99,
            "the page to reload with its lines back", 15,
        )
        checks["a reload brings back every line"] = after["last"] == before["last"]
        checks["a reload brings back the topic"] = after["topic"] == before["topic"]

        web = [pid for pid, comm in descendants(app.pid) if comm.startswith("WebKitWebProces")]
        checks["found this run's web process"] = len(web) == 1
        for pid in web:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        talker.send(f"PRIVMSG {channel} :while it was down {token}")
        recovered = probe.wait_until(
            lambda s: s["loadedAt"] != after["loadedAt"] and f"while it was down {token}" in s["last"],
            "the view to recover from a crashed web process", 20,
        )
        checks["a crashed web process is recovered"] = recovered["lines"] == before["lines"] + 1

        talker.send(f"TOPIC {channel} :")
        cleared = probe.wait_until(lambda s: s["topic"] == "(No Topic)", "the topic bar to clear", 10)
        checks["an empty topic shows (No Topic)"] = cleared["topic"] == "(No Topic)"

        snapshot = prefix.with_suffix(".png")
        checks["snapshot written"] = snapshot.exists() and snapshot.stat().st_size > 5000

        probe.trigger("close")
        quit_line = talker.wait_for(lambda l: l.startswith(f":{app_nick}!") and " QUIT " in l, 10)
        checks["closing the window quits cleanly"] = quit_line is not None and "Quit" in quit_line
        try:
            app.wait(timeout=10)
            checks["the app exits after closing"] = app.returncode == 0
        except subprocess.TimeoutExpired:
            checks["the app exits after closing"] = False

        for name, ok in checks.items():
            print(f"{'ok  ' if ok else 'FAIL'} {name}")
        print(json.dumps({k: v for k, v in before.items() if k != "lastHtml"}, indent=2))
        print(f"screenshot: {snapshot}")
        talker.send("QUIT :done")
        if not all(checks.values()):
            sys.exit(1)
        print("PASS")
    finally:
        if app.poll() is None:
            os.killpg(app.pid, signal.SIGTERM)
            try:
                app.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(app.pid, signal.SIGKILL)


if __name__ == "__main__":
    main()
