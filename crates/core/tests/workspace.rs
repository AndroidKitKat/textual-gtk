mod support;

use std::time::Instant;

use chrono::{Local, TimeZone};
use textual_core::irc::Message;
use textual_core::irc::caps::WANTED;
use textual_core::model::buffer::BufferId;
use textual_core::model::network::{
    Address, ConnId, Effect, Input as NetInput, ServerConfig, ServerId, Tls, TransportEvent,
};
use textual_core::model::workspace::{Input, Out, UiAction, Workspace};
use textual_core::model::{Change, Now};

fn config(id: u128, host: &str) -> ServerConfig {
    ServerConfig {
        id: ServerId(id),
        addresses: vec![Address {
            host: host.to_owned(),
            port: 6667,
        }],
        tls: Tls::Off,
        auto_reconnect: true,
        setup: support::setup("textual"),
        name: host.to_owned(),
        autojoin: Vec::new(),
        wanted_caps: WANTED.iter().map(|c| (*c).to_owned()).collect(),
    }
}

struct Harness {
    ws: Workspace,
    now: Now,
}

impl Harness {
    fn new() -> Self {
        Harness {
            ws: Workspace::new(),
            now: Now {
                mono: Instant::now(),
                wall: Local.timestamp_opt(1_800_000_000, 0).unwrap(),
            },
        }
    }

    fn input(&mut self, input: Input) -> Out {
        let mut out = Out::default();
        self.ws.update(input, self.now, &mut out);
        out
    }

    /// Connects `server` and registers it with no caps.
    fn register(&mut self, server: ServerId) -> ConnId {
        let out = self.input(Input::Network {
            server,
            input: NetInput::Connect,
        });
        let conn = out
            .effects
            .iter()
            .find_map(|(s, e)| match e {
                Effect::Dial { conn, .. } if *s == server => Some(*conn),
                _ => None,
            })
            .expect("dialed");
        self.event(server, conn, TransportEvent::Opened);
        self.line(server, conn, ":s CAP * LS :x");
        self.line(server, conn, ":s 001 textual :Welcome");
        conn
    }

    fn event(&mut self, server: ServerId, conn: ConnId, event: TransportEvent) -> Out {
        self.input(Input::Network {
            server,
            input: NetInput::Transport { conn, event },
        })
    }

    fn line(&mut self, server: ServerId, conn: ConnId, raw: &str) -> Out {
        let message = Message::parse(raw).unwrap();
        self.event(server, conn, TransportEvent::Line(message))
    }

    fn buffer(&self, server: ServerId, name: &str) -> BufferId {
        self.ws
            .network(server)
            .unwrap()
            .buffers()
            .iter()
            .find(|b| b.name() == name)
            .unwrap()
            .id()
    }
}

#[test]
fn two_servers_keep_their_own_buffers() {
    let mut h = Harness::new();
    let (a, b) = (ServerId(1), ServerId(2));
    let mut out = Out::default();
    h.ws.add_network(config(1, "a.test"), &mut out);
    h.ws.add_network(config(2, "b.test"), &mut out);
    let console_a = h.ws.network(a).unwrap().buffers().console().id();
    let console_b = h.ws.network(b).unwrap().buffers().console().id();
    assert_ne!(console_a, console_b);
    assert_eq!(
        out.changes,
        [
            Change::BufferAdded(console_a),
            Change::Focus(console_a),
            Change::BufferAdded(console_b)
        ],
        "only the first console takes the focus"
    );

    let conn_a = h.register(a);
    let conn_b = h.register(b);
    // Both networks number their connections from 1; the server id keeps
    // them apart.
    assert_eq!(conn_a, conn_b);
    let out = h.line(a, conn_a, ":textual!u@h JOIN #same");
    assert!(
        out.effects.iter().all(|(s, _)| *s == a),
        "{:?}",
        out.effects
    );
    h.line(b, conn_b, ":textual!u@h JOIN #same");
    let same_a = h.buffer(a, "#same");
    let same_b = h.buffer(b, "#same");
    assert_ne!(same_a, same_b);

    let out = h.line(b, conn_b, ":bob!b@h PRIVMSG #same :hi");
    let lines: Vec<_> = out
        .changes
        .iter()
        .filter(|c| matches!(c, Change::Line { .. } | Change::Counters(_)))
        .collect();
    assert!(
        matches!(
            lines.as_slice(),
            [Change::Line { buffer, .. }, Change::Counters(counted)]
                if *buffer == same_b && *counted == same_b
        ),
        "a line, then its counters: {lines:?}"
    );
    let (_, buffer) = h.ws.buffer(same_a).unwrap();
    assert_eq!(buffer.counters().unread, 0, "the other server's channel");

    // Focusing a buffer on b and collapsing a leaves the focus on b.
    h.input(Input::Ui(UiAction::Select(same_b)));
    let out = h.input(Input::Ui(UiAction::SetExpanded {
        server: a,
        expanded: false,
    }));
    assert_eq!(out.changes, [Change::Expanded(a)]);
    assert_eq!(h.ws.focus(), Some(same_b));

    // A collapsed server's console is its row, so selecting it leaves the
    // server collapsed; selecting a buffer under it expands it.
    let out = h.input(Input::Ui(UiAction::Select(console_a)));
    assert_eq!(out.changes, [Change::Focus(console_a)]);
    assert!(!h.ws.is_expanded(a));
    let out = h.input(Input::Ui(UiAction::Select(same_a)));
    assert_eq!(out.changes, [Change::Expanded(a), Change::Focus(same_a)]);
    assert!(h.ws.is_expanded(a));

    // An event for a server that does not exist is ignored.
    let out = h.line(ServerId(9), conn_a, ":bob!b@h PRIVMSG #same :hi");
    assert!(out.changes.is_empty() && out.effects.is_empty());
}

#[test]
fn line_ids_and_times_come_from_the_workspace() {
    let run = || {
        let mut h = Harness::new();
        let mut out = Out::default();
        h.ws.add_network(config(1, "a.test"), &mut out);
        let conn = h.register(ServerId(1));
        h.line(ServerId(1), conn, ":bob!b@h PRIVMSG textual :one");
        let id = h.buffer(ServerId(1), "bob");
        let (_, buffer) = h.ws.buffer(id).unwrap();
        buffer
            .scrollback()
            .lines()
            .map(|l| (l.id, l.received_at, l.plain_text()))
            .collect::<Vec<_>>()
    };
    let first = run();
    assert_eq!(first, run(), "two runs print equal lines");
    assert_eq!(first[0].1, Local.timestamp_opt(1_800_000_000, 0).unwrap());
}
