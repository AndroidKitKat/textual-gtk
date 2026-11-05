import json, os, re, shutil, sys, uuid, time, html as htmlmod
import gi
V41 = os.environ.get("POC_WEBKIT41") == "1"
if V41:
    gi.require_version("Gtk", "3.0"); gi.require_version("WebKit2", "4.1")
    from gi.repository import Gtk, GLib, WebKit2 as WebKit
else:
    gi.require_version("Gtk", "4.0"); gi.require_version("WebKit", "6.0")
    from gi.repository import Gtk, WebKit, GLib

SRC = os.path.expanduser("~/workspace/Textual-4f09312/Sources/App/Resources/Styling")
OUT = sys.argv[1]
STYLE = sys.argv[2] if len(sys.argv) > 2 else "Simplified"
VARIETY = sys.argv[3] if len(sys.argv) > 3 else "Light"
RES = os.path.join(OUT, "Resources")
CACHE = os.path.join(OUT, "cache", "Cached-Style-Resources")
TPL = os.path.join(RES, "Style Default Templates", "Version 4")

shutil.rmtree(OUT, ignore_errors=True)
shutil.copytree(os.path.join(SRC, "JavaScript"), os.path.join(RES, "JavaScript"))
shutil.copytree(os.path.join(SRC, "Style Default Templates"), os.path.join(RES, "Style Default Templates"))
shutil.copytree(os.path.join(SRC, "Bundled Styles", STYLE), CACHE)

TAG = re.compile(r"\{\{(\{|&|#|\^|/|!|>)?\s*(.*?)\s*\}?\}\}", re.S)

def parse(src):
    root, stack, pos = [], [], 0
    cur = root
    for m in TAG.finditer(src):
        cur.append(("text", src[pos:m.start()]))
        pos = m.end()
        kind, name = m.group(1), m.group(2)
        if kind == "!":
            continue
        if kind in ("#", "^"):
            node = (kind, name, [])
            cur.append(node)
            stack.append(cur)
            cur = node[2]
        elif kind == "/":
            cur = stack.pop()
        elif kind == ">":
            cur.append((">", name))
        elif kind in ("{", "&"):
            cur.append(("raw", name))
        else:
            cur.append(("esc", name))
    cur.append(("text", src[pos:]))
    return root

def lookup(ctx, name):
    m = re.fullmatch(r"isEmpty\((\w+)\)", name)
    if m:
        v = lookup(ctx, m.group(1))
        return v is None or v == "" or v == [] or v is False
    if name == ".":
        return ctx[-1]
    for c in reversed(ctx):
        if isinstance(c, dict) and name in c:
            return c[name]
    return None

def template(name):
    for d in [os.path.join(CACHE, "Varieties", VARIETY, "Templates"), os.path.join(CACHE, "Templates"), TPL]:
        p = os.path.join(d, name + ".mustache")
        if os.path.exists(p):
            return parse(open(p).read())
    raise KeyError(name)

def render(nodes, ctx):
    out = []
    for n in nodes:
        k = n[0]
        if k == "text":
            out.append(n[1])
        elif k == "esc":
            v = lookup(ctx, n[1])
            out.append(htmlmod.escape("" if v is None else fmt(v)))
        elif k == "raw":
            v = lookup(ctx, n[1])
            out.append("" if v is None else fmt(v))
        elif k == ">":
            out.append(render(template(n[1]), ctx))
        elif k == "#":
            v = lookup(ctx, n[1])
            if isinstance(v, list):
                for item in v:
                    out.append(render(n[2], ctx + [item]))
            elif v:
                out.append(render(n[2], ctx + [v]))
        elif k == "^":
            v = lookup(ctx, n[1])
            if not v:
                out.append(render(n[2], ctx))
    return "".join(out)

def fmt(v):
    if v is True: return "1"
    if v is False: return "0"
    return str(v)

def render_named(name, attrs):
    return render(template(name), [attrs]).replace("\n", "").replace("\r", "")

css_files, js_files = [], []
for d in [CACHE, os.path.join(CACHE, "Varieties", VARIETY)]:
    if os.path.exists(os.path.join(d, "design.css")): css_files.append(os.path.join(d, "design.css"))
    if os.path.exists(os.path.join(d, "scripts.js")): js_files.append(os.path.join(d, "scripts.js"))

tokens = {
    "userConfiguredFontName": "DejaVu Sans", "userConfiguredFontSize": 12 * 72 / 96,
    "nicknameIndentationAvailable": False,
    "applicationResourcePath": RES, "applicationTemplatesPath": TPL,
    "activeStyleAbsolutePath": CACHE,
    "activeStyleCSSFiles": css_files, "activeStyleJSFiles": js_files,
    "cacheToken": "123456", "configuredServerName": "TestNet",
    "isReloadingStyle": False, "operatingSystemVersion": "14.0",
    "appearanceDescription": "light", "sidebarInversionIsEnabled": False,
    "userConfiguredTextEncoding": "utf-8", "userStyleSheetRules": "",
    "usesCustomScrollers": False, "isChannelView": True, "isPrivateMessageView": False,
    "isUtilityView": False, "channelName": "#test", "viewTypeToken": "channel",
    "textDirectionToken": "ltr", "appearanceToken": "light",
}
doc = render_named("baseLayout", tokens)
doc_path = os.path.join(CACHE, str(uuid.uuid4()).upper() + ".html")
open(doc_path, "w").write(doc)

HANDLERS = """appearance channelIsActive channelMemberCount channelName channelNameDoubleClicked
displayContextMenu copySelectionWhenPermitted encryptionAuthenticateUser inlineMediaEnabledForView
loadInlineMedia localUserHostmask localUserNickname logToConsole networkName nicknameColorStyleHash
nicknameDoubleClicked notifyLinesAddedToView notifyLinesRemovedFromView notifyJumpToLineCallback
printDebugInformation printDebugInformationToConsole renderMessagesBefore renderMessagesAfter
renderMessagesInRange renderMessageWithSiblings renderTemplate retrievePreferencesWithMethodName
sendPluginPayload serverAddress serverChannelCount serverIsConnected setChannelName setNickname
setSelection setURLAddress sidebarInversionIsEnabled styleSettingsRetrieveValue styleSettingsSetValue
topicBarDoubleClicked finishedLayingOutView""".split()
if os.environ.get("POC_HANDLERS") is not None:
    HANDLERS = os.environ["POC_HANDLERS"].split()

REPLIES = {"appearance": "light", "channelIsActive": True, "channelMemberCount": 3, "channelName": "#test",
           "inlineMediaEnabledForView": False, "localUserHostmask": "me!u@h", "localUserNickname": "me",
           "networkName": "TestNet", "serverAddress": "localhost", "serverChannelCount": 1,
           "serverIsConnected": True, "sidebarInversionIsEnabled": False, "styleSettingsRetrieveValue": None,
           "styleSettingsSetValue": False, "renderMessagesBefore": [], "renderMessagesAfter": [],
           "renderMessagesInRange": [], "renderMessageWithSiblings": [], "nicknameColorStyleHash": None,
           "renderTemplate": None, "retrievePreferencesWithMethodName": None, "copySelectionWhenPermitted": False}

received = {}
results = {}
app = Gtk.Application(application_id="org.example.textualpoc")

def js(view, code, cb=None):
    def done(v, res):
        try:
            val = v.evaluate_javascript_finish(res)
            if cb: cb(val.to_json(0) if val and not val.is_undefined() else None)
        except Exception as e:
            print("JSERR", code[:80], e, flush=True)
            if cb: cb(None)
    view.evaluate_javascript(code, -1, None, None, None, done)

def call(view, fn, *args, cb=None):
    js(view, "%s(%s);" % (fn, ",".join(json.dumps(a) for a in args)), cb)

def on_activate(app):
    win = Gtk.ApplicationWindow(application=app, default_width=800, default_height=500)
    view = WebKit.WebView()
    s = view.get_settings()
    s.set_allow_file_access_from_file_urls(True)
    s.set_allow_universal_access_from_file_urls(True)
    s.set_enable_developer_extras(True)
    s.set_enable_write_console_messages_to_stdout(True)
    ucm = view.get_user_content_manager()

    def on_msg(mgr, value, name):
        if V41: value = value.get_js_value()
        body = json.loads(value.to_json(0)) if not value.is_undefined() else None
        received[name] = received.get(name, 0) + 1
        if isinstance(body, dict) and "promiseIndex" in body:
            call(view, "appInternal.promiseKept", body["promiseIndex"], REPLIES.get(name))
    for h in HANDLERS:
        ucm.connect("script-message-received::" + h, on_msg, h)
        ucm.register_script_message_handler(h) if V41 else ucm.register_script_message_handler(h, None)

    state = {"loaded": False, "n": 0}

    def append_line():
        n = state["n"]; state["n"] += 1
        ln = uuid.uuid4().hex[:4].upper() + "-" + uuid.uuid4().hex[:12].upper()
        privmsg = n % 5 != 4
        attrs = {"timestamp": time.time(), "formattedTimestamp": time.strftime("%H:%M:%S"),
                 "localizedTimestamp": time.ctime(), "lineType": "privmsg" if privmsg else "join",
                 "command": "privmsg" if privmsg else "join", "rawCommand": "privmsg",
                 "lineClassAttribute": "text" if privmsg else "event", "highlightAttribute": "false",
                 "message": "hello %d" % n, "formattedMessage": htmlmod.escape("hello <world> line %d" % n),
                 "isHighlight": False, "isRemoteMessage": n % 3 != 0, "inlineMediaEnabled": False,
                 "lineNumber": ln, "lineRenderTime": time.time(), "configuredServerName": "TestNet",
                 "activeStyleAbsolutePath": CACHE, "applicationResourcePath": RES}
        if privmsg:
            nick = ["alice", "bob", "me"][n % 3]
            attrs.update({"isNicknameAvailable": True, "nicknameColorStyle": "hsl(%d,60%%,40%%)" % (n * 47 % 360),
                          "nicknameColorStyleOverride": False, "nicknameColorHashingEnabled": True,
                          "formattedNickname": "<%s>" % nick, "nickname": nick,
                          "nicknameType": "myself" if nick == "me" else "normal"})
        if n == 0:
            attrs.update({"showDateIndicator": True, "dateIndicatorMessage": "Today"})
        tname = "newMessagePostedWithSender" if privmsg else "newMessagePostedWithoutSender"
        call(view, "MessageBuffer.bufferElementAppend", render_named(tname, attrs), [ln])
        if state["n"] < 80:
            return True
        GLib.timeout_add(1500, check)
        return False

    def check():
        probe = """JSON.stringify({lines: document.querySelectorAll('#messageBuffer div.line').length,
          bodyOpacity: document.getElementById('body').style.opacity,
          loadingOpacity: document.getElementById('loadingScreen').style.opacity,
          topic: document.getElementById('topicBar').innerText,
          scrolledToBottom: TextualScroller.isScrolledToBottom(),
          scrollTop: Textual.documentBodyElement().scrollTop,
          scrollHeight: Textual.documentBodyElement().scrollHeight,
          isWebKit2: app.isWebKit2(), hidden: document.hidden,
          observer: _TextualScroller._mutationObserver !== null,
          tracked: ConversationTracking.trackedNicknames.length,
          selected: document.body.dataset.selected,
          automatic: typeof TextualScroller.performScroll})"""
        def got(v):
            results["probe"] = json.loads(json.loads(v)) if v else None
            results["received"] = received
            print("RESULT", json.dumps(results, indent=1), flush=True)
            def snap(v2, res):
                tex = v2.get_snapshot_finish(res)
                (tex.write_to_png if V41 else tex.save_to_png)(os.path.join(OUT, "snapshot.png"))
                app.quit()
            if V41:
                gi.require_version("Gdk", "3.0")
                from gi.repository import Gdk
                w = view.get_toplevel().get_window()
                Gdk.pixbuf_get_from_window(w, 0, 0, w.get_width(), w.get_height()).savev(os.path.join(OUT, "snapshot.png"), "png", [], [])
                app.quit()
            else:
                view.get_snapshot(WebKit.SnapshotRegion.VISIBLE, WebKit.SnapshotOptions.NONE, None, snap)
        js(view, probe, got)
        return False

    def finished_loading():
        call(view, "Textual.viewInitiated", "channel", "client-1", "channel-1", "#test")
        call(view, "_Textual.viewFinishedLoading", {"selected": True, "visible": True, "reloadingTheme": False,
                                                    "textSizeMultiplier": 1.0, "scrollbackLimit": 0})
        call(view, "Textual.setTopicBarValue", "Welcome to #test", "Welcome to <b>#test</b>")
        call(view, "_Textual.viewFinishedLoadingHistory")
        GLib.timeout_add(30, append_line)
        return False

    def on_load(v, ev):
        if ev == WebKit.LoadEvent.FINISHED and not state["loaded"]:
            state["loaded"] = True
            GLib.timeout_add(1200, finished_loading)
    view.connect("load-changed", on_load)
    view.connect("load-failed", lambda v, e, uri, err: print("LOADFAIL", uri, err, flush=True))
    view.connect("web-process-terminated", lambda v, r: print("TERMINATED", r, flush=True))

    if V41:
        win.add(view); win.show_all()
    else:
        win.set_child(view); win.present()
    view.load_uri("file://" + doc_path)

app.connect("activate", on_activate)
app.run([])
