//! Carries out the model's `Effect`s on tokio: dials, sends and hangups.
//! Every connection lives under its (server, conn) pair, and what it hears
//! comes back into the app's inbox tagged with its server.

pub mod transport;

use std::collections::HashMap;

use textual_core::model::network::{self, ConnId, Effect, ServerId, TransportEvent};
use textual_core::model::workspace::Input;
use tokio::sync::mpsc;

use crate::app::{Inbox, Request};

pub struct Io {
    runtime: tokio::runtime::Handle,
    inbox: Inbox,
    connections: HashMap<(ServerId, ConnId), transport::Connection>,
    /// The transport reports `(ConnId, event)`; one task per server adds
    /// the `ServerId` on the way to the inbox.
    forwarders: HashMap<ServerId, transport::Events>,
}

impl Io {
    pub fn new(runtime: tokio::runtime::Handle, inbox: Inbox) -> Self {
        Io {
            runtime,
            inbox,
            connections: HashMap::new(),
            forwarders: HashMap::new(),
        }
    }

    pub fn execute(&mut self, effects: Vec<(ServerId, Effect)>) {
        for (server, effect) in effects {
            match effect {
                Effect::Dial { conn, address, tls } => {
                    let events = self.events_for(server);
                    let connection = transport::spawn(
                        &self.runtime,
                        conn,
                        address,
                        tls,
                        transport::Settings::default(),
                        events,
                    );
                    self.connections.insert((server, conn), connection);
                }
                Effect::Send { conn, line } => {
                    if let Some(connection) = self.connections.get(&(server, conn)) {
                        connection.send(line);
                    }
                }
                Effect::Hangup { conn } => {
                    self.connections.remove(&(server, conn));
                }
            }
        }
    }

    fn events_for(&mut self, server: ServerId) -> transport::Events {
        if let Some(events) = self.forwarders.get(&server) {
            return events.clone();
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<(ConnId, TransportEvent)>();
        let inbox = self.inbox.clone();
        self.runtime.spawn(async move {
            while let Some((conn, event)) = rx.recv().await {
                let input = Input::Network {
                    server,
                    input: network::Input::Transport { conn, event },
                };
                if inbox.send(Request::Input(input)).is_err() {
                    return;
                }
            }
        });
        self.forwarders.insert(server, tx.clone());
        tx
    }
}
