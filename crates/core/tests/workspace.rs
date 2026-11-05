mod support;

use std::time::Instant;

use chrono::{Local, TimeZone};
use textual_core::irc::Message;
use textual_core::irc::caps::WANTED;
use textual_core::model::buffer::{BufferId, Rejoin};
use textual_core::model::network::{
    Address, ChannelConfig, ConnId, Effect, Input as NetInput, ServerConfig, ServerId, Tls,
    TransportEvent,
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

#[test]
fn configured_channels_are_parted_buffers_from_the_start() {
    let mut h = Harness::new();
    let mut cfg = config(1, "a.test");
    let channel = |name: &str| ChannelConfig {
        name: name.to_owned(),
        key: None,
    };
    // The same channel twice, spelled differently, is one buffer.
    cfg.autojoin = vec![channel("#a"), channel("#B"), channel("#A")];
    let mut out = Out::default();
    h.ws.add_network(cfg, &mut out);
    let net = h.ws.network(ServerId(1)).unwrap();
    let ids: Vec<BufferId> = net.buffers().iter().map(|b| b.id()).collect();
    assert_eq!(ids.len(), 3);
    assert_eq!(
        out.changes,
        [
            Change::BufferAdded(ids[0]),
            Change::BufferAdded(ids[1]),
            Change::BufferAdded(ids[2]),
            Change::Focus(ids[0]),
        ]
    );
    for b in net.buffers().iter().skip(1) {
        assert_eq!(b.rejoin(), Some(Rejoin::OnConnect));
        assert!(b.joined().is_none());
    }
    assert_eq!(net.next_wake(), None, "offline: nothing to wake for");
}

/// The PRIVMSG text of every line sent, in order.
fn sent_texts(out: &Out) -> Vec<String> {
    out.effects
        .iter()
        .filter_map(|(_, e)| match e {
            Effect::Send { line, .. } => Some(line.as_str().trim_end().to_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_long_line_is_cut_to_what_the_server_relays() {
    let mut h = Harness::new();
    let server = ServerId(1);
    h.ws.add_network(config(1, "irc.test"), &mut Out::default());
    let conn = h.register(server);
    let words: Vec<String> = (0..300).map(|i| format!("w{i}")).collect();
    let text = words.join(" ");
    let typed = |h: &mut Harness, buffer| {
        h.input(Input::Ui(UiAction::Submit {
            buffer,
            text: text.clone(),
            confirmed: false,
        }))
    };
    h.line(
        server,
        conn,
        ":textual!~me@a.very.long.host.example JOIN #c",
    );
    let c = h.buffer(server, "#c");
    let parts = |out: &Out| -> Vec<String> {
        sent_texts(out)
            .into_iter()
            .filter_map(|l| l.strip_prefix("PRIVMSG #c :").map(str::to_owned))
            .collect()
    };
    let out = typed(&mut h, c);
    let known = parts(&out);
    assert!(known.len() > 1);
    for part in &known {
        let relayed = format!(":textual!~me@a.very.long.host.example PRIVMSG #c :{part}\r\n");
        assert!(relayed.len() <= 512, "{}", relayed.len());
    }
    // The space a part was cut at starts the next part.
    assert!(known[1].starts_with(' '));
    assert_eq!(known.concat(), text);
    // Each part is printed as it is sent.
    let lines = h.ws.buffer(c).unwrap().1.scrollback().len();
    assert!(lines >= known.len());

    // Before our host is known, the longest one is assumed.
    let mut h = Harness::new();
    h.ws.add_network(config(1, "irc.test"), &mut Out::default());
    let conn = h.register(server);
    h.line(server, conn, ":textual JOIN #c");
    let c = h.buffer(server, "#c");
    let unknown = parts(&typed(&mut h, c));
    assert!(unknown.len() >= known.len());
    for part in &unknown {
        assert!(
            part.len() <= 512 - (1 + 7 + 1 + 74 + 9 + 2 + 2) - 2,
            "{}",
            part.len()
        );
    }
    assert_eq!(unknown.concat(), text);
}

/// Every line sent, byte for byte, CRLF included.
fn sent_raw(out: &Out) -> Vec<String> {
    out.effects
        .iter()
        .filter_map(|(_, e)| match e {
            Effect::Send { line, .. } => Some(line.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn raw_and_unknown_commands_go_out_as_typed() {
    let mut h = Harness::new();
    let server = ServerId(1);
    h.ws.add_network(config(1, "irc.test"), &mut Out::default());
    h.register(server);
    let console = h.buffer(server, "irc.test");
    let mut typed = |text: &str| {
        let out = h.input(Input::Ui(UiAction::Submit {
            buffer: console,
            text: text.to_owned(),
            confirmed: true,
        }));
        sent_raw(&out)
    };
    // Textual's `sendLine:` with the typed string untouched.
    assert_eq!(typed("/raw PRIVMSG #x :two  "), ["PRIVMSG #x :two  \r\n"]);
    assert_eq!(typed("quote  WHOIS  a  "), ["WHOIS  a  \r\n"]);
    // Unknown: `UPPERCASE rest`, with the space even when there is no rest
    // (`sendCommand:withData:`), and any characters in the command.
    assert_eq!(typed("/test-x y "), ["TEST-X y \r\n"]);
    assert_eq!(typed("version"), ["VERSION \r\n"]);
    assert_eq!(typed("/ümlaut"), ["ÜMLAUT \r\n"]);
    // What would end the line early or smuggle another is still refused.
    assert!(typed("/raw PRIVMSG #x :a\0b").is_empty());
}
