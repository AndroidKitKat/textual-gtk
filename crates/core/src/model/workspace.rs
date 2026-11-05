//! Everything the client knows: the networks with their buffers, which
//! buffer is focused, and which servers are collapsed in the sidebar. The
//! app feeds it `Input`s and carries out the `Effect`s; the view reads it
//! and patches widgets from the `Change`s.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::input::completion::Completion;
use crate::input::history::History;
use crate::model::buffer::{Buffer, BufferId};
use crate::model::network::{self, Effect, Network, ServerConfig, ServerId};
use crate::model::{Change, Cx, Ids, Now, Preferences};

mod submit;

#[derive(Debug)]
pub enum Input {
    Network {
        server: ServerId,
        input: network::Input,
    },
    Ui(UiAction),
    /// The time from `next_wake` has come.
    Wake,
}

/// What the user did in the window. Selection, expansion, window state,
/// close and draft are idempotent, so the echo of a change the model made
/// itself costs nothing; the input keys are not echoes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    Select(BufferId),
    SetExpanded {
        server: ServerId,
        expanded: bool,
    },
    WindowActive(bool),
    Close(BufferId),
    /// Return in the input of `buffer`. The view has already emptied the
    /// field, as Textual does before anything is sent. `confirmed` is the
    /// answer to `UiEffect::ConfirmFlood`.
    Submit {
        buffer: BufferId,
        text: String,
        confirmed: bool,
    },
    /// The input's text changed.
    Draft(String),
    /// Up (`older`) or Down in history, with the field's text.
    History {
        text: String,
        older: bool,
    },
    /// Tab or Shift+Tab (`backward`). `selection` is in characters.
    Complete {
        text: String,
        selection: (usize, usize),
        backward: bool,
    },
}

/// Something only the view can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEffect {
    /// Put `text` in the input with the caret at `caret` (in characters),
    /// unless the user changed the field from `was` meanwhile.
    SetInput {
        was: String,
        text: String,
        caret: usize,
    },
    /// Textual's "possible flood" question for text typed into `buffer`; a
    /// yes sends it again as `Submit { confirmed: true }`.
    ConfirmFlood { buffer: BufferId, text: String },
}

#[derive(Debug, Default)]
pub struct Out {
    pub effects: Vec<(ServerId, Effect)>,
    pub changes: Vec<Change>,
    pub ui: Vec<UiEffect>,
}

/// The input field's memory: Textual's `TLOInputHistory` and its
/// `TLONicknameCompletionStatus`.
#[derive(Debug, Default)]
struct InputMemory {
    /// What the field holds, as the view last reported it.
    field: String,
    global: History,
    /// With `input_history_per_buffer`: each buffer's history and unsent
    /// text (`lastHistoryItem`).
    buffers: HashMap<BufferId, (History, String)>,
    completion: Completion,
}

#[derive(Debug)]
pub struct Workspace {
    /// In sidebar order.
    networks: Vec<Network>,
    focus: Option<BufferId>,
    window_active: bool,
    collapsed: HashSet<ServerId>,
    ids: Ids,
    prefs: Preferences,
    input: InputMemory,
}

impl Default for Workspace {
    fn default() -> Self {
        Workspace::new()
    }
}

impl Workspace {
    pub fn new() -> Self {
        Workspace::with_preferences(Preferences::default())
    }

    pub fn with_preferences(prefs: Preferences) -> Self {
        Workspace {
            networks: Vec::new(),
            focus: None,
            window_active: true,
            collapsed: HashSet::new(),
            ids: Ids::default(),
            prefs,
            input: InputMemory::default(),
        }
    }

    /// Adds a server, offline, at the end of the sidebar. The first one's
    /// console takes the focus.
    pub fn add_network(&mut self, config: ServerConfig, out: &mut Out) {
        assert!(self.network(config.id).is_none(), "server ids are unique");
        let network = Network::new(config, &mut self.ids, &mut out.changes);
        let console = network.buffers().console().id();
        self.networks.push(network);
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
    fn on_network<R>(
        &mut self,
        server: ServerId,
        now: Now,
        out: &mut Out,
        step: impl FnOnce(&mut Network, &mut Cx, &mut network::Out) -> R,
    ) -> Option<R> {
        let net = self.networks.iter_mut().find(|n| n.id() == server)?;
        let mut cx = Cx {
            now,
            ids: &mut self.ids,
            prefs: &self.prefs,
        };
        let mut net_out = network::Out::default();
        let result = step(net, &mut cx, &mut net_out);
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
        Some(result)
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
            UiAction::Close(id) => self.close(id, now, out),
            UiAction::Submit {
                buffer,
                text,
                confirmed,
            } => self.submit(buffer, &text, confirmed, now, out),
            UiAction::Draft(text) => self.input.field = text,
            UiAction::History { text, older } => self.recall(text, older, out),
            UiAction::Complete {
                text,
                selection,
                backward,
            } => self.complete(text, selection.0..selection.1, backward, now, out),
        }
    }

    /// Closes a channel or query; the focus falls back to the row above.
    fn close(&mut self, id: BufferId, now: Now, out: &mut Out) {
        let (Some(server), Some(previous)) = (self.server_of(id), self.previous_buffer(id)) else {
            return;
        };
        self.on_network(server, now, out, |net, _, o| net.close(id, o));
        if self.buffer(id).is_some() {
            return;
        }
        // Textual's `TLOInputHistory destroy:`.
        self.input.buffers.remove(&id);
        // The row above always exists: the console is never closed.
        if self.focus == Some(id) {
            self.focus = None;
            self.set_focus(previous, out);
        }
    }

    /// Up or Down: Textual's `moveInputHistory:`. Down leaves the caret at
    /// the start, Up at the end.
    fn recall(&mut self, text: String, older: bool, out: &mut Out) {
        let history = match self.focus {
            Some(id) if self.prefs.input_history_per_buffer => {
                &mut self.input.buffers.entry(id).or_default().0
            }
            _ => &mut self.input.global,
        };
        let recalled = if older {
            history.up(&text)
        } else {
            history.down(&text)
        };
        if let Some(new) = recalled {
            let caret = if older { new.chars().count() } else { 0 };
            self.set_input(text, new, caret, out);
        }
    }

    fn set_input(&mut self, was: String, text: String, caret: usize, out: &mut Out) {
        self.input.field.clone_from(&text);
        out.ui.push(UiEffect::SetInput { was, text, caret });
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
        // the sidebar can show the selection. The console is the server
        // row, which a collapse leaves visible.
        let console = self.buffer(id).is_some_and(|(_, b)| b.is_console());
        if !console && self.collapsed.remove(&server) {
            out.changes.push(Change::Expanded(server));
        }
        self.set_focus(id, out);
    }

    /// Moves the focus. With per-buffer history the field's text stays
    /// with the buffer it was typed in (`TLOInputHistory moveFocusTo:`).
    fn set_focus(&mut self, id: BufferId, out: &mut Out) {
        if self.focus == Some(id) {
            return;
        }
        if self.prefs.input_history_per_buffer {
            let field = std::mem::take(&mut self.input.field);
            if let Some(old) = self.focus {
                self.input
                    .buffers
                    .entry(old)
                    .or_default()
                    .1
                    .clone_from(&field);
            }
            let draft = self
                .input
                .buffers
                .get(&id)
                .map(|(_, d)| d.clone())
                .unwrap_or_default();
            let caret = draft.chars().count();
            self.set_input(field, draft, caret, out);
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
            self.set_focus(console, out);
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
