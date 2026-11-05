#!/usr/bin/env python3
"""End-to-end check for the client.

Starts the real app under Xvfb on two networks of the local IRC server
(plain and TLS), and drives it through the TEXTUAL_GTK_PROBE hook: the app
writes what the window shows (sidebar rows with classes and badges, member
rows, title, the focused page's DOM) to a JSON file once a second and takes
clicks from an actions file. A second and third client talk to it. Checks:
autojoin on both networks, the message path through the page, badges and
highlights on an unfocused channel, the member list following NAMES, JOIN,
MODE and PART, collapsing the focused buffer's server, a page reload, a
killed web process, and a clean QUIT from both nicks when the window closes.

    scripts/e2e.py [--server 127.0.0.1:6667] [--tls-port 6697] [--out DIR]
                   [--binary PATH]

Needs: a built target/debug/textual-gtk, xvfb-run, dbus-run-session, an
IRC server (the local Ergo: scripts/test-server.sh), and on Ubuntu 24.04+
the AppArmor profile from scripts/dev-setup.sh, without which WebKit's
sandbox cannot start.
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

# A private session bus with no activatable services: the desktop's
# portals would otherwise be started inside it and stall GTK's start-up
# for 25 s while they wait for a keyring.
BUS_CONFIG = """<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"""


def fail(message):
    print(f"FAIL: {message}")
    sys.exit(1)


class Client:
    def __init__(self, host, port, nick):
        self.sock = socket.create_connection((host, port), timeout=10)
        self.buf = b""
        self.nick = nick
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

    def join(self, channel):
        self.send(f"JOIN {channel}")
        if self.wait_for(lambda l: l.startswith(f":{self.nick}!") and f"JOIN {channel}" in l) is None:
            fail(f"{self.nick} could not join {channel}")

    def names(self, channel):
        """The nicks the server lists for `channel`, prefixes stripped."""
        self.send(f"NAMES {channel}")
        nicks = []
        while True:
            line = self.wait_for(lambda l: f" 353 {self.nick} " in l or f" 366 {self.nick} {channel} " in l)
            if line is None:
                fail(f"no NAMES reply for {channel}")
            if " 366 " in line:
                return nicks
            for token in line.split(" :", 1)[1].split():
                nicks.append(token.lstrip("~&@%+"))


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

    def act(self, *actions):
        path = self.prefix.with_suffix(".actions")
        path.write_text("".join(json.dumps(a) + "\n" for a in actions))

    def wait_until(self, predicate, what, timeout=20):
        deadline = time.time() + timeout
        state = None
        while time.time() < deadline:
            if self.app.poll() is not None:
                fail(f"the app exited with {self.app.returncode} while waiting for {what}; see {self.log}")
            state = self.state()
            try:
                if state and predicate(state):
                    return state
            except (KeyError, TypeError, IndexError):
                pass
            time.sleep(0.2)
        fail(f"{what} never happened; last probe: {json.dumps(state)[:2000]}")

    def wait_page(self, predicate, what, timeout=20):
        return self.wait_until(lambda s: s["page"] and predicate(s["page"]), what, timeout)["page"]

    def wait_view(self, predicate, what, timeout=20):
        return self.wait_until(lambda s: predicate(s["view"]), what, timeout)["view"]


def row(view, row_id):
    return next((r for r in view["rows"] if r["id"] == row_id), None)


def descendants(root):
    """Pids under `root`, so only this run's processes are touched."""
    table = subprocess.run(["ps", "-e", "-o", "pid=,ppid=,comm="], capture_output=True, text=True).stdout
    children = {}
    for line in table.splitlines():
        pid, ppid, comm = line.split(None, 2)
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
    parser.add_argument("--tls-port", type=int, default=6697)
    parser.add_argument("--out", default=str(ROOT / "target/e2e"))
    parser.add_argument("--binary", default=str(ROOT / "target/debug/textual-gtk"))
    args = parser.parse_args()
    host, port = args.server.rsplit(":", 1)
    port = int(port)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    prefix = out / "probe"
    for suffix in (".json", ".png", ".reload", ".close", ".actions"):
        prefix.with_suffix(suffix).unlink(missing_ok=True)
    log = out / "app.log"
    bus_config = out / "bus.conf"
    bus_config.write_text(BUS_CONFIG)

    token = secrets.token_hex(4)
    channel, channel2, channel3 = (f"#e2e-{token}", f"#e2e-{token}-b", f"#e2e-{token}-s")
    app_nick, app_nick_tls = f"app{token}", f"app{token}s"
    talker_nick, peer_nick = f"talk{token}", f"peer{token}"
    binary = Path(args.binary)
    if not binary.exists():
        fail(f"{binary} is missing; run cargo build")

    # The talker makes the channels first, so it holds ops for the MODE
    # check, and sits in all three to see both nicks' QUIT at the end.
    talker = Client(host, port, talker_nick)
    for c in (channel, channel2, channel3):
        talker.join(c)

    env = dict(os.environ, TEXTUAL_GTK_PROBE=str(prefix), GSK_RENDERER="cairo", GTK_A11Y="none")
    app = subprocess.Popen(
        ["dbus-run-session", f"--config-file={bus_config}", "--",
         "xvfb-run", "-a", "-s", "-screen 0 1100x700x24", str(binary),
         "--network", f"name=Local,host={host},port={port},tls=off,nick={app_nick},join={channel} {channel2}",
         "--network", f"name=Secure,host={host},port={args.tls_port},tls=on,verify=false,nick={app_nick_tls},join={channel3}"],
        env=env, stdout=open(log, "w"), stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    probe = Probe(prefix, app, log)
    checks = {}
    try:
        for nick, c in ((app_nick, channel), (app_nick, channel2), (app_nick_tls, channel3)):
            joined = talker.wait_for(
                lambda l, nick=nick, c=c: l.startswith(f":{nick}!") and f"JOIN {c}" in l,
                45, alive=lambda: app.poll() is None,
            )
            if joined is None:
                hint = " (is the AppArmor profile from scripts/dev-setup.sh loaded?)" if "bwrap" in log.read_text() else ""
                fail(f"{nick} never joined {c}{hint}; see {log}")

        view = probe.wait_view(
            lambda v: all("state-joined" in r["classes"] for r in v["rows"] if r["kind"] == "server")
            and all("state-joined" in r["classes"] for r in v["rows"] if r["kind"] == "buffer"),
            "both networks to come online with their channels joined", 30,
        )
        checks["both server rows show state-joined"] = [r["id"] for r in view["rows"] if "state-joined" in r["classes"] and r["kind"] == "server"] == ["Local", "Secure"]
        checks["channel rows sit under their server, in order"] = [r["id"] for r in view["rows"]] == [
            "Local", f"Local/{channel}", f"Local/{channel2}", "Secure", f"Secure/{channel3}"]
        checks["the app starts on the first console"] = view["focus"] == "Local" and row(view, "Local")["selected"]
        network = view["subtitle"].split(" on ")[-1]
        checks["the console title names the network and nick"] = view["subtitle"] == f"{app_nick} on {network}" and view["title"] == network

        probe.act({"select": f"Local/{channel}"})
        # The app asks for the channel's modes on join; the 324 reply is
        # paced behind its other lines, and must be in before the page's
        # last line is compared.
        view = probe.wait_view(lambda v: v["focus"] == f"Local/{channel}" and "[+" in v["title"], "the channel to take the focus")
        checks["the selected row follows the focus"] = row(view, f"Local/{channel}")["selected"]
        checks["the input is live on an online channel"] = view["input_sensitive"]
        checks["the channel title counts its users and modes"] = view["title"].startswith(f"{channel} (2 users) [+")

        talker.send(f"TOPIC {channel} :topic \x02{token}\x02")
        text = f"hello from e2e {token} <script>x</script>"
        talker.send(f"PRIVMSG {channel} :\x02bold\x02 {text}")
        probe.wait_page(lambda p: text in p["last"], "the message to show up")
        time.sleep(2)
        state = probe.state()["page"]
        checks["the message is the last line"] = text in state["last"]
        checks["the sender is shown"] = f"<{talker_nick}>" in state["last"]
        checks["html in the message stays text"] = "<script" not in state["lastHtml"]
        checks["formatting codes render as markup"] = "<b>bold</b>" in state["lastHtml"]
        checks["no control characters reach the text"] = not any(ord(c) < 0x20 for c in state["last"])
        checks["the join line came first"] = state["lines"] >= 2
        checks["the style faded the page in"] = float(state["bodyOpacity"]) > 0.99
        checks["the topic bar shows the topic"] = state["topic"].replace("\xa0", " ") == f"topic {token}"
        checks["a message in the focused channel leaves no badge"] = row(probe.state()["view"], f"Local/{channel}")["badge"] == ""

        talker.send(f"PRIVMSG {channel2} :{app_nick}: psst")
        view = probe.wait_view(lambda v: row(v, f"Local/{channel2}")["badge"] == "1", "the highlight badge")
        unfocused = row(view, f"Local/{channel2}")
        checks["a highlight in an unfocused channel marks its row"] = {"has-unread", "has-highlight"} <= set(unfocused["classes"])
        talker.send(f"PRIVMSG {channel2} :plain words")
        time.sleep(2)
        unfocused = row(probe.state()["view"], f"Local/{channel2}")
        checks["the badge keeps the highlight count over the unread count"] = unfocused["badge"] == "1" and "has-unread" in unfocused["classes"]
        probe.act({"select": f"Local/{channel2}"})
        view = probe.wait_view(lambda v: v["focus"] == f"Local/{channel2}", "channel2 to take the focus")
        cleared = row(view, f"Local/{channel2}")
        checks["selecting the channel clears its badge and classes"] = cleared["badge"] == "" and not {"has-unread", "has-highlight"} & set(cleared["classes"])

        probe.act({"select": f"Local/{channel}"})
        view = probe.wait_view(lambda v: v["focus"] == f"Local/{channel}", "the channel to take the focus again")
        names = talker.names(channel)
        checks["the member list matches NAMES"] = sorted(m["nick"] for m in view["members"]) == sorted(names) and view["members_visible"]
        peer = Client(host, port, peer_nick)
        peer.join(channel)
        view = probe.wait_view(lambda v: peer_nick in [m["nick"] for m in v["members"]], "the peer's join in the member list")
        checks["the member list follows a JOIN"] = [m["nick"] for m in view["members"]][-1] == peer_nick
        checks["the title follows the user count"] = view["title"].startswith(f"{channel} (3 users)")
        talker.send(f"MODE {channel} +o {peer_nick}")
        view = probe.wait_view(
            lambda v: next(m for m in v["members"] if m["nick"] == peer_nick)["prefix"] == "@",
            "the peer's op in the member list",
        )
        opped = next(m for m in view["members"] if m["nick"] == peer_nick)
        checks["an op gets the @ prefix, rank-o and moves to the top"] = "rank-o" in opped["classes"] and view["members"][0]["nick"] in (peer_nick, talker_nick) and all(m["prefix"] == "@" for m in view["members"][:2])
        peer.send(f"PART {channel}")
        view = probe.wait_view(lambda v: peer_nick not in [m["nick"] for m in v["members"]], "the peer's part in the member list")
        checks["the member list follows a PART"] = sorted(m["nick"] for m in view["members"]) == sorted(names)

        probe.act({"collapse": "Local"})
        view = probe.wait_view(lambda v: v["focus"] == "Local", "the collapse to move the focus to the console")
        checks["collapsing the focused buffer's server focuses its console"] = row(view, "Local")["selected"] and not row(view, "Local")["expanded"]
        checks["a collapsed server hides its channels"] = [r["id"] for r in view["rows"]] == ["Local", "Secure", f"Secure/{channel3}"]
        checks["the console hides the member list"] = not view["members_visible"] and view["members"] == []
        probe.act({"expand": "Local"})
        view = probe.wait_view(lambda v: row(view, "Local") and len(v["rows"]) == 5, "the expand to bring the rows back")
        checks["expanding brings the channels back"] = [r["id"] for r in view["rows"]] == [
            "Local", f"Local/{channel}", f"Local/{channel2}", "Secure", f"Secure/{channel3}"] and view["focus"] == "Local"

        probe.act({"select": f"Local/{channel}"})
        probe.wait_view(lambda v: v["focus"] == f"Local/{channel}", "the channel to take the focus for the reload")
        time.sleep(1.5)
        before = probe.state()["page"]
        probe.trigger("reload")
        after = probe.wait_page(
            lambda p: p["loadedAt"] != before["loadedAt"]
            and p["lines"] == before["lines"]
            and float(p["bodyOpacity"]) > 0.99,
            "the page to reload with its lines back", 15,
        )
        checks["a reload brings back every line"] = after["last"] == before["last"]
        checks["a reload brings back the topic"] = after["topic"] == before["topic"]

        web = [pid for pid, comm in descendants(app.pid) if comm.startswith("WebKitWebProces")]
        checks["found this run's one shared web process"] = len(web) == 1
        for pid in web:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        talker.send(f"PRIVMSG {channel} :while it was down {token}")
        recovered = probe.wait_page(
            lambda p: p["loadedAt"] != after["loadedAt"] and f"while it was down {token}" in p["last"],
            "the view to recover from a crashed web process", 20,
        )
        checks["a crashed web process is recovered"] = recovered["lines"] == before["lines"] + 1

        talker.send(f"TOPIC {channel} :")
        cleared = probe.wait_page(lambda p: p["topic"] == "(No Topic)", "the topic bar to clear", 10)
        checks["an empty topic shows (No Topic)"] = cleared["topic"] == "(No Topic)"

        snapshot = prefix.with_suffix(".png")
        checks["snapshot written"] = snapshot.exists() and snapshot.stat().st_size > 5000

        probe.trigger("close")
        quits = {}
        deadline = time.time() + 10
        while len(quits) < 2 and time.time() < deadline:
            line = talker.wait_for(lambda l: " QUIT " in l and (l.startswith(f":{app_nick}!") or l.startswith(f":{app_nick_tls}!")), deadline - time.time())
            if line is None:
                break
            quits[line.split("!", 1)[0][1:]] = line
        checks["closing the window quits both networks cleanly"] = set(quits) == {app_nick, app_nick_tls} and all("Quit" in l for l in quits.values())
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
        peer.send("QUIT :done")
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
