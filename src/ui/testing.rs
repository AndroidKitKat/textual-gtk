//! A workspace driven by hand for the view's unit tests: no socket, no
//! clock, no GTK.

use std::time::Instant;

use chrono::{Local, TimeZone};
use textual_core::irc::Message;
use textual_core::irc::caps::WANTED;
use textual_core::model::Now;
use textual_core::model::buffer::Buffer;
use textual_core::model::network::{
    Address, CloseReason, ConnId, Effect, Input as NetInput, Network, ServerConfig, ServerId, Tls,
    TransportEvent,
};
use textual_core::model::registration::{Auth, Setup};
use textual_core::model::workspace::{Input, Out, UiAction, Workspace};

pub struct Harness {
    pub ws: Workspace,
    pub now: Now,
}

impl Harness {
    pub fn new() -> Self {
        Harness {
            ws: Workspace::new(),
            now: Now {
                mono: Instant::now(),
                wall: Local.timestamp_opt(1_800_000_000, 0).unwrap(),
            },
        }
    }

    pub fn add_network(&mut self, name: &str) -> ServerId {
        let server = ServerId(self.ws.networks().len() as u128 + 1);
        let config = ServerConfig {
            id: server,
            name: name.to_owned(),
            addresses: vec![Address {
                host: "127.0.0.1".into(),
                port: 6667,
            }],
            tls: Tls::Off,
            auto_reconnect: true,
            setup: Setup {
                nick: "textual".into(),
                alternates: Vec::new(),
                username: "textual".into(),
                realname: "Textual".into(),
                auth: Auth::None,
                server_password: None,
                account_password: None,
            },
            autojoin: Vec::new(),
            wanted_caps: WANTED.iter().map(|c| (*c).to_owned()).collect(),
        };
        self.ws.add_network(config, &mut Out::default());
        server
    }

    pub fn network(&self, server: ServerId) -> &Network {
        self.ws.network(server).expect("added")
    }

    pub fn buffer(&self, server: ServerId, name: &str) -> &Buffer {
        self.network(server)
            .buffers()
            .iter()
            .find(|b| b.name() == name)
            .unwrap_or_else(|| panic!("no buffer {name}"))
    }

    pub fn input(&mut self, input: Input) -> Out {
        let mut out = Out::default();
        self.ws.update(input, self.now, &mut out);
        out
    }

    pub fn connect(&mut self, server: ServerId) -> ConnId {
        let out = self.input(Input::Network {
            server,
            input: NetInput::Connect,
        });
        out.effects
            .iter()
            .find_map(|(_, e)| match e {
                Effect::Dial { conn, .. } => Some(*conn),
                _ => None,
            })
            .expect("dialed")
    }

    pub fn event(&mut self, server: ServerId, conn: ConnId, event: TransportEvent) -> Out {
        self.input(Input::Network {
            server,
            input: NetInput::Transport { conn, event },
        })
    }

    pub fn line(&mut self, server: ServerId, conn: ConnId, raw: &str) -> Out {
        let message = Message::parse(raw).unwrap();
        self.event(server, conn, TransportEvent::Line(message))
    }

    /// Registers a dialed link with no caps.
    pub fn register(&mut self, server: ServerId, conn: ConnId) {
        self.event(server, conn, TransportEvent::Opened);
        self.line(server, conn, ":s CAP * LS :x");
        self.line(server, conn, ":s 001 textual :Welcome");
    }

    /// A user disconnect, with the server closing `conn` after the QUIT.
    pub fn disconnect(&mut self, server: ServerId, conn: ConnId) {
        self.input(Input::Network {
            server,
            input: NetInput::Disconnect { reason: None },
        });
        self.event(server, conn, TransportEvent::Closed(CloseReason::Eof));
    }

    pub fn select(&mut self, server: ServerId, name: &str) {
        let id = self.buffer(server, name).id();
        self.input(Input::Ui(UiAction::Select(id)));
    }
}
