//! The window title: what Textual's `updateTitle` shows for the focused
//! buffer, minus the topic, which belongs to the chat view.

use std::time::Instant;

use textual_core::model::buffer::Body;
use textual_core::model::network::Link;
use textual_core::model::workspace::Workspace;

/// (title, subtitle) for the focused buffer at `now`.
pub fn title(ws: &Workspace, now: Instant) -> (String, String) {
    let Some((net, buf)) = ws.focus().and_then(|id| ws.buffer(id)) else {
        return ("Textual GTK".to_owned(), String::new());
    };
    let network = net
        .session()
        .and_then(|s| s.isupport().network.as_deref())
        .unwrap_or(&net.config().name);
    let title = match (buf.body(), buf.joined()) {
        (Body::Console, _) => network.to_owned(),
        (Body::Channel(_), Some(joined)) => {
            let mut title = format!("{} ({} users)", buf.name(), joined.members().len());
            let modes = joined.modes();
            if modes != "+" {
                title.push_str(&format!(" [{modes}]"));
            }
            title
        }
        (Body::Channel(_) | Body::Query, _) => buf.name().to_owned(),
    };
    let subtitle = match net.link() {
        Link::Online { session, .. } => format!("{} on {network}", session.nick()),
        Link::Connecting { address, .. } => format!("Connecting to {address}"),
        Link::Registering { .. } => format!("Logging in to {network}"),
        Link::Waiting { until, .. } => {
            let left = until.saturating_duration_since(now).as_secs_f64().ceil();
            format!("Reconnecting in {left}s")
        }
        Link::Quitting { .. } => "Quitting".to_owned(),
        Link::Offline { .. } => "Disconnected".to_owned(),
    };
    (title, subtitle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::testing::Harness;

    #[test]
    fn the_title_follows_the_focus_and_the_link() {
        let mut h = Harness::new();
        let server = h.add_network("Local");
        let now = h.now.mono;
        assert_eq!(title(&h.ws, now), ("Local".into(), "Disconnected".into()));
        let conn = h.connect(server);
        assert_eq!(
            title(&h.ws, now),
            ("Local".into(), "Connecting to 127.0.0.1:6667".into())
        );
        h.event(
            server,
            conn,
            textual_core::model::network::TransportEvent::Opened,
        );
        assert_eq!(
            title(&h.ws, now),
            ("Local".into(), "Logging in to Local".into())
        );
        h.line(server, conn, ":s CAP * LS :x");
        h.line(server, conn, ":s 001 textual :Welcome");
        assert_eq!(
            title(&h.ws, now),
            ("Local".into(), "textual on Local".into())
        );
        h.line(server, conn, ":s 005 textual NETWORK=Ergo :are supported");
        assert_eq!(title(&h.ws, now), ("Ergo".into(), "textual on Ergo".into()));

        h.line(server, conn, ":textual!t@h JOIN #a");
        h.line(server, conn, ":s 353 textual = #a :@textual peer");
        h.line(server, conn, ":s 366 textual #a :End");
        h.select(server, "#a");
        assert_eq!(
            title(&h.ws, now),
            ("#a (2 users)".into(), "textual on Ergo".into())
        );
        h.line(server, conn, ":s 324 textual #a +nt");
        assert_eq!(
            title(&h.ws, now),
            ("#a (2 users) [+nt]".into(), "textual on Ergo".into())
        );

        h.line(server, conn, ":peer!p@h PRIVMSG textual :hi");
        h.select(server, "peer");
        assert_eq!(title(&h.ws, now), ("peer".into(), "textual on Ergo".into()));

        h.line(server, conn, "ERROR :Closing Link: killed");
        assert_eq!(
            title(&h.ws, now),
            ("peer".into(), "Reconnecting in 5s".into())
        );
        h.disconnect(server, conn);
        assert_eq!(title(&h.ws, now), ("peer".into(), "Disconnected".into()));
    }
}
