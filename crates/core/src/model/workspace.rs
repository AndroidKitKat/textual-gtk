//! Everything the client knows: the networks with their buffers, which
//! buffer is focused, and which servers are collapsed in the sidebar. The
//! app feeds it `Input`s and carries out the `Effect`s; the view reads it
//! and patches widgets from the `Change`s.

use std::collections::HashSet;
use std::time::Instant;

use crate::model::buffer::{Buffer, BufferId};
use crate::model::network::{self, Effect, Network, ServerConfig, ServerId};
use crate::model::{Change, Cx, Ids, Now};

#[derive(Debug)]
pub enum Input {
    Network {
        server: ServerId,
        input: network::Input,
    },
    /// Text typed into a buffer, sent as one PRIVMSG.
    Say {
        buffer: BufferId,
        text: String,
    },
    Ui(UiAction),
    /// The time from `next_wake` has come.
    Wake,
}

/// What the user did in the window. Each is idempotent, so the echo of a
/// selection the model made itself costs nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    Select(BufferId),
    SetExpanded { server: ServerId, expanded: bool },
    WindowActive(bool),
    Close(BufferId),
}

#[derive(Debug, Default)]
pub struct Out {
    pub effects: Vec<(ServerId, Effect)>,
    pub changes: Vec<Change>,
}

#[derive(Debug)]
pub struct Workspace {
    /// In sidebar order.
    networks: Vec<Network>,
    focus: Option<BufferId>,
    window_active: bool,
    collapsed: HashSet<ServerId>,
    ids: Ids,
}

impl Default for Workspace {
    fn default() -> Self {
        Workspace::new()
    }
}

impl Workspace {
    pub fn new() -> Self {
        Workspace {
            networks: Vec::new(),
            focus: None,
            window_active: true,
            collapsed: HashSet::new(),
            ids: Ids::default(),
        }
    }

    /// Adds a server, offline, at the end of the sidebar. The first one's
    /// console takes the focus.
    pub fn add_network(&mut self, config: ServerConfig, out: &mut Out) {
        assert!(self.network(config.id).is_none(), "server ids are unique");
        let console = self.ids.buffer();
        self.networks.push(Network::new(config, console));
        out.changes.push(Change::BufferAdded(console));
        if self.focus.is_none() {
            self.focus = Some(console);
            out.changes.push(Change::Focus(console));
        }
    }

    pub fn networks(&self) -> &[Network] {
        &self.networks
    }

    pub fn network(&self, server: ServerId) -> Option<&Network> {
        self.networks.iter().find(|n| n.id() == server)
    }

    /// A buffer and the network it belongs to.
    pub fn buffer(&self, id: BufferId) -> Option<(&Network, &Buffer)> {
        self.networks
            .iter()
            .find_map(|n| n.buffers().get(id).map(|b| (n, b)))
    }

    pub fn focus(&self) -> Option<BufferId> {
        self.focus
    }

    pub fn window_active(&self) -> bool {
        self.window_active
    }

    pub fn is_expanded(&self, server: ServerId) -> bool {
        !self.collapsed.contains(&server)
    }

    pub fn next_wake(&self) -> Option<Instant> {
        self.networks.iter().filter_map(Network::next_wake).min()
    }

    pub fn update(&mut self, input: Input, now: Now, out: &mut Out) {
        match input {
            Input::Network { server, input } => {
                self.on_network(server, now, out, |net, cx, o| net.update(input, cx, o));
            }
            Input::Say { buffer, text } => {
                if let Some(server) = self.server_of(buffer) {
                    self.on_network(server, now, out, |net, cx, o| net.say(buffer, &text, cx, o));
                }
            }
            Input::Wake => {
                let servers: Vec<ServerId> = self.networks.iter().map(Network::id).collect();
                for server in servers {
                    self.on_network(server, now, out, |net, cx, o| {
                        net.update(network::Input::Wake, cx, o);
                    });
                }
            }
            Input::Ui(action) => self.on_ui(action, now, out),
        }
    }

    fn server_of(&self, buffer: BufferId) -> Option<ServerId> {
        self.buffer(buffer).map(|(n, _)| n.id())
    }

    /// Runs one step of one network, then counts the lines it printed.
    fn on_network(
        &mut self,
        server: ServerId,
        now: Now,
        out: &mut Out,
        step: impl FnOnce(&mut Network, &mut Cx, &mut network::Out),
    ) {
        let Some(net) = self.networks.iter_mut().find(|n| n.id() == server) else {
            return;
        };
        let mut cx = Cx {
            now,
            ids: &mut self.ids,
        };
        let mut net_out = network::Out::default();
        step(net, &mut cx, &mut net_out);
        out.effects
            .extend(net_out.effects.into_iter().map(|e| (server, e)));
        let seen = self.focus.filter(|_| self.window_active);
        for change in net_out.changes {
            let counted = match &change {
                Change::Line { buffer, line } if Some(*buffer) != seen => net
                    .buffers_mut()
                    .get_mut(*buffer)
                    .is_some_and(|b| b.count(line))
                    .then_some(*buffer),
                _ => None,
            };
            out.changes.push(change);
            if let Some(buffer) = counted {
                out.changes.push(Change::Counters(buffer));
            }
        }
    }

    fn on_ui(&mut self, action: UiAction, now: Now, out: &mut Out) {
        match action {
            UiAction::Select(id) => self.select(id, out),
            UiAction::SetExpanded { server, expanded } => self.set_expanded(server, expanded, out),
            UiAction::WindowActive(active) => {
                self.window_active = active;
                if active && let Some(id) = self.focus {
                    self.mark_read(id, out);
                }
            }
            UiAction::Close(id) => {
                let (Some(server), Some(previous)) = (self.server_of(id), self.previous_buffer(id))
                else {
                    return;
                };
                self.on_network(server, now, out, |net, _, o| net.close(id, o));
                // The row above always exists: the console is never closed.
                if self.focus == Some(id) && self.buffer(id).is_none() {
                    self.focus = Some(previous);
                    out.changes.push(Change::Focus(previous));
                    if self.window_active {
                        self.mark_read(previous, out);
                    }
                }
            }
        }
    }

    /// The buffer above `id` in its network, which a close falls back to.
    fn previous_buffer(&self, id: BufferId) -> Option<BufferId> {
        let (net, _) = self.buffer(id)?;
        let ids: Vec<BufferId> = net.buffers().iter().map(Buffer::id).collect();
        let at = ids.iter().position(|b| *b == id)?;
        at.checked_sub(1).map(|i| ids[i])
    }

    fn select(&mut self, id: BufferId, out: &mut Out) {
        if self.focus == Some(id) {
            return;
        }
        let Some(server) = self.server_of(id) else {
            return;
        };
        // Focusing a buffer under a collapsed server expands it first, so
        // the sidebar can show the selection.
        if self.collapsed.remove(&server) {
            out.changes.push(Change::Expanded(server));
        }
        self.focus = Some(id);
        out.changes.push(Change::Focus(id));
        if self.window_active {
            self.mark_read(id, out);
        }
    }

    fn set_expanded(&mut self, server: ServerId, expanded: bool, out: &mut Out) {
        let Some(console) = self.network(server).map(|n| n.buffers().console().id()) else {
            return;
        };
        let changed = if expanded {
            self.collapsed.remove(&server)
        } else {
            self.collapsed.insert(server)
        };
        if !changed {
            return;
        }
        out.changes.push(Change::Expanded(server));
        // Collapsing hides the focused buffer's row, so the focus moves to
        // the server row, which is its console.
        if !expanded
            && let Some(focus) = self.focus
            && focus != console
            && self.server_of(focus) == Some(server)
        {
            self.focus = Some(console);
            out.changes.push(Change::Focus(console));
            if self.window_active {
                self.mark_read(console, out);
            }
        }
    }

    fn mark_read(&mut self, id: BufferId, out: &mut Out) {
        let cleared = self
            .networks
            .iter_mut()
            .find_map(|n| n.buffers_mut().get_mut(id))
            .is_some_and(Buffer::mark_read);
        if cleared {
            out.changes.push(Change::Counters(id));
        }
    }
}
