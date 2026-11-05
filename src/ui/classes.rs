//! The CSS class names the later "Textual for Mac" stylesheet keys on.
//! Nothing else in the view writes a class string.

use textual_core::model::buffer::{Body, Buffer};
use textual_core::model::members::MemberRow;
use textual_core::model::network::{Ended, Link, Network};

pub const WINDOW: &str = "tx-window";
pub const SIDEBAR: &str = "tx-sidebar";
pub const SERVER_ROW: &str = "tx-server-row";
pub const BUFFER_ROW: &str = "tx-buffer-row";
pub const BADGE: &str = "tx-badge";
pub const MEMBER_LIST: &str = "tx-member-list";
pub const MEMBER_ROW: &str = "tx-member-row";
pub const INPUT: &str = "tx-input";
pub const FLOOD_QUESTION: &str = "tx-flood-question";

const KIND_CHANNEL: &str = "kind-channel";
const KIND_QUERY: &str = "kind-query";
const STATE_JOINED: &str = "state-joined";
const STATE_PARTED: &str = "state-parted";
const STATE_OFFLINE: &str = "state-offline";
const STATE_ERROR: &str = "state-error";
const HAS_UNREAD: &str = "has-unread";
const HAS_HIGHLIGHT: &str = "has-highlight";
const IS_AWAY: &str = "is-away";

pub fn server_row_classes(net: &Network) -> Vec<&'static str> {
    let state = match net.link() {
        Link::Online { .. } => STATE_JOINED,
        Link::Waiting { .. } => STATE_ERROR,
        Link::Offline { last: Some(why) } if *why != Ended::UserQuit => STATE_ERROR,
        Link::Offline { .. }
        | Link::Quitting { .. }
        | Link::Connecting { .. }
        | Link::Registering { .. } => STATE_OFFLINE,
    };
    vec![SERVER_ROW, state]
}

pub fn buffer_row_classes(net: &Network, buf: &Buffer) -> Vec<&'static str> {
    let mut classes = vec![BUFFER_ROW];
    match buf.body() {
        Body::Console => {}
        Body::Channel(_) => {
            classes.push(KIND_CHANNEL);
            classes.push(if buf.joined().is_some() {
                STATE_JOINED
            } else {
                STATE_PARTED
            });
        }
        Body::Query => {
            classes.push(KIND_QUERY);
            if !matches!(net.link(), Link::Online { .. }) {
                classes.push(STATE_OFFLINE);
            }
        }
    }
    let counters = buf.counters();
    if counters.unread > 0 {
        classes.push(HAS_UNREAD);
    }
    if counters.highlights > 0 {
        classes.push(HAS_HIGHLIGHT);
    }
    classes
}

pub fn member_row_classes(row: &MemberRow) -> Vec<String> {
    let mut classes = vec![MEMBER_ROW.to_owned()];
    if let Some((mode, _)) = row.prefix {
        classes.push(format!("rank-{mode}"));
    }
    if row.away {
        classes.push(IS_AWAY.to_owned());
    }
    classes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::testing::Harness;
    use textual_core::model::members::MemberRow;

    #[test]
    fn a_server_row_tracks_its_link() {
        let mut h = Harness::new();
        let server = h.add_network("Local");
        assert_eq!(
            server_row_classes(h.network(server)),
            [SERVER_ROW, STATE_OFFLINE]
        );
        let conn = h.connect(server);
        assert_eq!(
            server_row_classes(h.network(server)),
            [SERVER_ROW, STATE_OFFLINE],
            "connecting is not an error"
        );
        h.register(server, conn);
        assert_eq!(
            server_row_classes(h.network(server)),
            [SERVER_ROW, STATE_JOINED]
        );
        h.line(server, conn, "ERROR :Closing Link: killed");
        assert_eq!(
            server_row_classes(h.network(server)),
            [SERVER_ROW, STATE_ERROR],
            "waiting to retry"
        );
        h.disconnect(server, conn);
        assert_eq!(
            server_row_classes(h.network(server)),
            [SERVER_ROW, STATE_OFFLINE],
            "offline after a user quit"
        );
    }

    #[test]
    fn a_channel_row_shows_kind_state_and_counters() {
        let mut h = Harness::new();
        let server = h.add_network("Local");
        let conn = h.connect(server);
        h.register(server, conn);
        h.line(server, conn, ":textual!t@h JOIN #a");
        h.line(server, conn, ":textual!t@h JOIN #b");
        h.select(server, "#b");
        let a = h.buffer(server, "#a");
        assert_eq!(
            buffer_row_classes(h.network(server), a),
            [BUFFER_ROW, KIND_CHANNEL, STATE_JOINED]
        );
        h.line(server, conn, ":peer!p@h PRIVMSG #a :hello");
        assert_eq!(
            buffer_row_classes(h.network(server), h.buffer(server, "#a")),
            [BUFFER_ROW, KIND_CHANNEL, STATE_JOINED, HAS_UNREAD]
        );
        h.line(server, conn, ":peer!p@h PRIVMSG #a :textual: hello");
        assert_eq!(
            buffer_row_classes(h.network(server), h.buffer(server, "#a")),
            [
                BUFFER_ROW,
                KIND_CHANNEL,
                STATE_JOINED,
                HAS_UNREAD,
                HAS_HIGHLIGHT
            ]
        );
        h.select(server, "#a");
        assert_eq!(
            buffer_row_classes(h.network(server), h.buffer(server, "#a")),
            [BUFFER_ROW, KIND_CHANNEL, STATE_JOINED],
            "focusing clears the counters"
        );
        h.line(server, conn, ":textual!t@h PART #a");
        assert_eq!(
            buffer_row_classes(h.network(server), h.buffer(server, "#a")),
            [BUFFER_ROW, KIND_CHANNEL, STATE_PARTED]
        );
    }

    #[test]
    fn a_query_row_goes_offline_with_its_link() {
        let mut h = Harness::new();
        let server = h.add_network("Local");
        let conn = h.connect(server);
        h.register(server, conn);
        h.line(server, conn, ":peer!p@h PRIVMSG textual :psst");
        assert_eq!(
            buffer_row_classes(h.network(server), h.buffer(server, "peer")),
            [BUFFER_ROW, KIND_QUERY, HAS_UNREAD]
        );
        h.disconnect(server, conn);
        assert_eq!(
            buffer_row_classes(h.network(server), h.buffer(server, "peer")),
            [BUFFER_ROW, KIND_QUERY, STATE_OFFLINE, HAS_UNREAD]
        );
    }

    #[test]
    fn a_member_row_names_its_rank_and_away_state() {
        let row = |prefix, away| MemberRow {
            nick: "n".into(),
            prefix,
            away,
        };
        assert_eq!(member_row_classes(&row(None, false)), [MEMBER_ROW]);
        assert_eq!(
            member_row_classes(&row(Some(('o', '@')), false)),
            [MEMBER_ROW, "rank-o"]
        );
        assert_eq!(
            member_row_classes(&row(Some(('v', '+')), true)),
            [MEMBER_ROW, "rank-v", IS_AWAY]
        );
    }
}
