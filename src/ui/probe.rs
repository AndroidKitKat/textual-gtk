//! The end-to-end hook, on when `TEXTUAL_GTK_PROBE=<prefix>`: once a second
//! the app writes what the window shows to `<prefix>.json` and a snapshot
//! to `<prefix>.png`. Files the test creates drive it: `<prefix>.reload`
//! reloads the focused chat view, `<prefix>.close` closes the window, and
//! `<prefix>.actions` (one JSON object per line) is fed in as clicks.

use std::path::{Path, PathBuf};

use gtk::prelude::*;
use serde_json::{Value, json};
use textual_core::model::buffer::BufferId;
use textual_core::model::network::ServerId;
use textual_core::model::workspace::{Input, UiAction, Workspace};

use crate::app::{Inbox, Request};
use crate::ui::view::View;

/// The page state the phase-1 check already reads from the focused view.
const PAGE_STATE: &str = "({\
    lines: document.querySelectorAll('#messageBuffer > .line').length,\
    last: (document.querySelector('#messageBuffer > .line:last-child') || {}).innerText || '',\
    lastHtml: (document.querySelector('#messageBuffer > .line:last-child') || {}).innerHTML || '',\
    bodyOpacity: getComputedStyle(document.getElementById('body')).opacity,\
    topic: (document.getElementById('topicBar') || {}).innerText || '',\
    hasTopicBar: !!document.getElementById('topicBar'),\
    viewType: document.body.dataset.viewType || null,\
    loadedAt: performance.timeOrigin\
})";

/// What one second's look at the trigger files found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tick {
    pub reload: bool,
    pub close: bool,
    pub actions: Vec<Action>,
}

/// A click the test asks for. Ids are `<network>/<buffer>`, or
/// `<network>` for a console or a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Select(String),
    Expand(String),
    Collapse(String),
    Close(String),
    Say(String, String),
    WindowActive(bool),
}

pub fn install(prefix: PathBuf, tx: Inbox) {
    gtk::glib::timeout_add_seconds_local(1, move || {
        let trigger = |suffix: &str| std::fs::remove_file(prefix.with_extension(suffix)).is_ok();
        let tick = Tick {
            reload: trigger("reload"),
            close: trigger("close"),
            actions: take_actions(&prefix.with_extension("actions")),
        };
        match tx.send(Request::Probe(tick)) {
            Ok(()) => gtk::glib::ControlFlow::Continue,
            Err(_) => gtk::glib::ControlFlow::Break,
        }
    });
}

fn take_actions(path: &Path) -> Vec<Action> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let _ = std::fs::remove_file(path);
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| match parse_action(line) {
            Ok(action) => Some(action),
            Err(e) => {
                eprintln!("textual-gtk: probe action {line:?} ignored: {e}");
                None
            }
        })
        .collect()
}

fn parse_action(line: &str) -> Result<Action, String> {
    let value: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
    let object = value.as_object().ok_or("not an object")?;
    let (key, arg) = object.iter().next().ok_or("empty object")?;
    let text = || arg.as_str().map(str::to_owned).ok_or("expected a string");
    Ok(match key.as_str() {
        "select" => Action::Select(text()?),
        "expand" => Action::Expand(text()?),
        "collapse" => Action::Collapse(text()?),
        "close" => Action::Close(text()?),
        "say" => {
            let pair = arg.as_array().ok_or("expected [id, text]")?;
            match (
                pair.first().and_then(Value::as_str),
                pair.get(1).and_then(Value::as_str),
            ) {
                (Some(id), Some(text)) => Action::Say(id.to_owned(), text.to_owned()),
                _ => return Err("expected [id, text]".into()),
            }
        }
        "window_active" => Action::WindowActive(arg.as_bool().ok_or("expected a bool")?),
        other => return Err(format!("unknown action {other}")),
    })
}

/// Turns an action into the input a click would send, or says which id it
/// could not find.
pub fn input_for(ws: &Workspace, action: &Action) -> Result<Input, String> {
    let buffer = |id: &str| buffer_by_path(ws, id).ok_or_else(|| format!("no buffer {id}"));
    let server = |id: &str| server_by_name(ws, id).ok_or_else(|| format!("no network {id}"));
    Ok(match action {
        Action::Select(id) => Input::Ui(UiAction::Select(buffer(id)?)),
        Action::Close(id) => Input::Ui(UiAction::Close(buffer(id)?)),
        Action::Expand(id) => Input::Ui(UiAction::SetExpanded {
            server: server(id)?,
            expanded: true,
        }),
        Action::Collapse(id) => Input::Ui(UiAction::SetExpanded {
            server: server(id)?,
            expanded: false,
        }),
        Action::Say(id, text) => Input::Say {
            buffer: buffer(id)?,
            text: text.clone(),
        },
        Action::WindowActive(active) => Input::Ui(UiAction::WindowActive(*active)),
    })
}

fn server_by_name(ws: &Workspace, name: &str) -> Option<ServerId> {
    ws.networks()
        .iter()
        .find(|n| n.config().name == name)
        .map(|n| n.id())
}

fn buffer_by_path(ws: &Workspace, path: &str) -> Option<BufferId> {
    let (network, buffer) = path.split_once('/').unwrap_or((path, ""));
    let net = ws.networks().iter().find(|n| n.config().name == network)?;
    if buffer.is_empty() {
        return Some(net.buffers().console().id());
    }
    net.buffers()
        .iter()
        .find(|b| !b.is_console() && b.name() == buffer)
        .map(|b| b.id())
}

fn path_of(ws: &Workspace, id: BufferId) -> Option<String> {
    let (net, buf) = ws.buffer(id)?;
    Some(if buf.is_console() {
        net.config().name.clone()
    } else {
        format!("{}/{}", net.config().name, buf.name())
    })
}

/// Writes `<prefix>.json` and `<prefix>.png` from the window as it is now.
pub fn write(prefix: &Path, ws: &Workspace, view: &View) {
    let rows: Vec<Value> = view
        .sidebar()
        .rows()
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id, "kind": r.kind, "label": r.label, "classes": r.classes,
                "badge": r.badge, "selected": r.selected, "expanded": r.expanded,
                "depth": r.depth,
            })
        })
        .collect();
    let members: Vec<Value> = view
        .members()
        .rows()
        .into_iter()
        .map(|m| json!({"nick": m.nick, "prefix": m.prefix, "classes": m.classes}))
        .collect();
    let state = json!({
        "focus": ws.focus().and_then(|id| path_of(ws, id)),
        "window_classes": view.window().css_classes().iter().map(|c| c.to_string()).collect::<Vec<_>>(),
        "title": view.title_widget().title().to_string(),
        "subtitle": view.title_widget().subtitle().to_string(),
        "input_sensitive": view.entry().is_sensitive(),
        "rows": rows,
        "members": members,
        "members_visible": view.members().widget().is_visible(),
    });
    let json_path = prefix.with_extension("json");
    match view.focused_chat() {
        Some(chat) => chat.query(PAGE_STATE, move |page| {
            let page = page
                .and_then(|p| serde_json::from_str(&p).ok())
                .unwrap_or(Value::Null);
            write_atomically(&json_path, &json!({"page": page, "view": state}));
        }),
        None => write_atomically(&json_path, &json!({"page": Value::Null, "view": state})),
    }
    snapshot(view, &prefix.with_extension("png"));
}

/// A reader that catches a half-written file would parse garbage.
fn write_atomically(path: &Path, value: &Value) {
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, value.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// The whole window, rendered off the window's own renderer; the focused
/// web view alone if that gives nothing. A hidden window (closing) keeps
/// the last picture.
fn snapshot(view: &View, path: &Path) {
    let window = view.window();
    if !window.is_visible() {
        return;
    }
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let (width, height) = (paintable.intrinsic_width(), paintable.intrinsic_height());
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let texture = snapshot
        .to_node()
        .zip(window.renderer())
        .map(|(node, renderer)| renderer.render_texture(&node, None));
    if let Some(texture) = texture.filter(|_| width > 0 && height > 0) {
        let _ = texture.save_to_png(path);
        return;
    }
    let Some(chat) = view.focused_chat() else {
        return;
    };
    let path = path.to_owned();
    webkit6::prelude::WebViewExt::snapshot(
        &chat.widget(),
        webkit6::SnapshotRegion::Visible,
        webkit6::SnapshotOptions::NONE,
        None::<&gtk::gio::Cancellable>,
        move |texture| {
            if let Ok(texture) = texture {
                let _ = texture.save_to_png(&path);
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::testing::Harness;

    #[test]
    fn actions_parse_and_resolve_against_the_workspace() {
        let mut h = Harness::new();
        let local = h.add_network("Local");
        let conn = h.connect(local);
        h.register(local, conn);
        h.line(local, conn, ":textual!t@h JOIN #a");
        let a = h.buffer(local, "#a").id();
        let console = h.network(local).buffers().console().id();

        let cases = [
            (r#"{"select":"Local/#a"}"#, Input::Ui(UiAction::Select(a))),
            (
                r#"{"select":"Local"}"#,
                Input::Ui(UiAction::Select(console)),
            ),
            (
                r#"{"collapse":"Local"}"#,
                Input::Ui(UiAction::SetExpanded {
                    server: local,
                    expanded: false,
                }),
            ),
            (r#"{"close":"Local/#a"}"#, Input::Ui(UiAction::Close(a))),
            (
                r#"{"say":["Local/#a","hi"]}"#,
                Input::Say {
                    buffer: a,
                    text: "hi".into(),
                },
            ),
            (
                r#"{"window_active":false}"#,
                Input::Ui(UiAction::WindowActive(false)),
            ),
        ];
        for (line, want) in cases {
            let action = parse_action(line).unwrap();
            let got = input_for(&h.ws, &action).unwrap();
            assert_eq!(format!("{got:?}"), format!("{want:?}"), "{line}");
        }
        assert!(input_for(&h.ws, &Action::Select("Local/#zz".into())).is_err());
        assert!(input_for(&h.ws, &Action::Expand("Nope".into())).is_err());
        assert!(parse_action(r#"{"dance":"Local"}"#).is_err());
        assert_eq!(path_of(&h.ws, a).as_deref(), Some("Local/#a"));
        assert_eq!(path_of(&h.ws, console).as_deref(), Some("Local"));
    }
}
