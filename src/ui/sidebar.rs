//! The server list: a tree of server rows (the console is the server row)
//! with their channel and query rows. Selection and expansion belong to the
//! model; the widgets only report clicks and follow `Change`s.

use std::collections::HashMap;

use gtk::prelude::*;
use gtk::{gio, glib};
use textual_core::model::buffer::BufferId;
use textual_core::model::network::ServerId;
use textual_core::model::workspace::{UiAction, Workspace};

use crate::app::Inbox;
use crate::ui::classes;
use crate::ui::rows::{Bound, BufferRow, RowText, ServerRow, buffer_of};

pub struct Sidebar {
    widget: gtk::ScrolledWindow,
    root: gio::ListStore,
    tree: gtk::TreeListModel,
    selection: gtk::SingleSelection,
    servers: HashMap<ServerId, ServerRow>,
    buffers: HashMap<BufferId, BufferRow>,
}

/// One visible row, for the probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowInfo {
    pub id: String,
    pub kind: &'static str,
    pub label: String,
    pub classes: Vec<String>,
    pub badge: String,
    pub selected: bool,
    pub expanded: bool,
    pub depth: u32,
}

impl Sidebar {
    pub fn build(tx: &Inbox) -> Self {
        let root = gio::ListStore::new::<ServerRow>();
        let tree = gtk::TreeListModel::new(root.clone(), false, false, |item| {
            item.downcast_ref::<ServerRow>()
                .map(|server| server.children().upcast())
        });
        let selection = gtk::SingleSelection::builder()
            .model(&tree)
            .autoselect(false)
            .can_unselect(false)
            .build();
        // `selected-item`, not `selection-changed`: when the selected row
        // disappears (a collapse, a close) GTK drops the selection and
        // emits only this notify, which the model then resolves.
        selection.connect_selected_item_notify({
            let tx = tx.clone();
            move |selection| {
                let id = selection
                    .selected_item()
                    .and_downcast::<gtk::TreeListRow>()
                    .and_then(|row| row.item())
                    .and_then(|item| buffer_of(&item));
                if let Some(id) = id {
                    let _ = tx.send(UiAction::Select(id).into());
                }
            }
        });
        let list = gtk::ListView::new(Some(selection.clone()), Some(factory(tx)));
        list.add_css_class(classes::SIDEBAR);
        let widget = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        Sidebar {
            widget,
            root,
            tree,
            selection,
            servers: HashMap::new(),
            buffers: HashMap::new(),
        }
    }

    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.widget
    }

    /// A console becomes a server row; anything else a child row at the
    /// model's position among that network's other buffers.
    pub fn add_buffer(&mut self, ws: &Workspace, id: BufferId) {
        let Some((net, buf)) = ws.buffer(id) else {
            return;
        };
        if buf.is_console() {
            let row = ServerRow::new(net.id(), id, &server_text(ws, net.id()));
            self.root.append(&row);
            if let Some(tree_row) = self.server_tree_row(&row) {
                tree_row.set_expanded(ws.is_expanded(net.id()));
            }
            self.servers.insert(net.id(), row);
            return;
        }
        let Some(server) = self.servers.get(&net.id()) else {
            return;
        };
        let at = net
            .buffers()
            .iter()
            .position(|b| b.id() == id)
            .expect("the buffer is in its network") as u32;
        let row = BufferRow::new(
            id,
            &RowText {
                label: buf.name().to_owned(),
                badge: badge(buf.counters()),
                classes: owned(classes::buffer_row_classes(net, buf)),
            },
        );
        server.children().insert(at - 1, &row);
        self.buffers.insert(id, row);
    }

    pub fn remove_buffer(&mut self, id: BufferId) {
        let Some(row) = self.buffers.remove(&id) else {
            return;
        };
        for server in self.servers.values() {
            let children = server.children();
            if let Some(at) = children.find(&row) {
                children.remove(at);
                return;
            }
        }
    }

    pub fn refresh_buffer(&self, ws: &Workspace, id: BufferId) {
        let (Some(row), Some((net, buf))) = (self.buffers.get(&id), ws.buffer(id)) else {
            return;
        };
        row.update(&RowText {
            label: buf.name().to_owned(),
            badge: badge(buf.counters()),
            classes: owned(classes::buffer_row_classes(net, buf)),
        });
    }

    /// The server row and, since a query's state follows the link, every
    /// row under it.
    pub fn refresh_server(&self, ws: &Workspace, server: ServerId) {
        let (Some(row), Some(net)) = (self.servers.get(&server), ws.network(server)) else {
            return;
        };
        row.update(&server_text(ws, server));
        for buf in net.buffers().iter() {
            self.refresh_buffer(ws, buf.id());
        }
    }

    /// Moves the selection to `id`'s row, expanding its server first since
    /// a collapsed child has no row to select.
    pub fn select(&self, ws: &Workspace, id: BufferId) {
        let Some(row) = self.tree_row(ws, id) else {
            return;
        };
        self.selection.set_selected(row.position());
    }

    pub fn set_expanded(&self, server: ServerId, expanded: bool) {
        if let Some(row) = self
            .servers
            .get(&server)
            .and_then(|row| self.server_tree_row(row))
        {
            row.set_expanded(expanded);
        }
    }

    /// The rows as the list shows them, collapsed children absent.
    pub fn rows(&self) -> Vec<RowInfo> {
        let selected = self.selection.selected();
        (0..self.selection.n_items())
            .filter_map(|position| {
                let row = self
                    .selection
                    .item(position)?
                    .downcast::<gtk::TreeListRow>()
                    .ok()?;
                let item = row.item()?;
                let (id, kind, label, classes, badge) =
                    if let Some(server) = item.downcast_ref::<ServerRow>() {
                        (
                            server.label(),
                            "server",
                            server.label(),
                            server.classes(),
                            server.badge(),
                        )
                    } else {
                        let buffer = item.downcast_ref::<BufferRow>()?;
                        let network = row
                            .parent()
                            .and_then(|p| p.item())
                            .and_downcast::<ServerRow>()
                            .map(|s| s.label())
                            .unwrap_or_default();
                        (
                            format!("{network}/{}", buffer.label()),
                            "buffer",
                            buffer.label(),
                            buffer.classes(),
                            buffer.badge(),
                        )
                    };
                Some(RowInfo {
                    id,
                    kind,
                    label,
                    classes,
                    badge,
                    selected: position == selected,
                    expanded: row.is_expanded(),
                    depth: row.depth(),
                })
            })
            .collect()
    }

    fn server_tree_row(&self, row: &ServerRow) -> Option<gtk::TreeListRow> {
        self.tree.child_row(self.root.find(row)?)
    }

    fn tree_row(&self, ws: &Workspace, id: BufferId) -> Option<gtk::TreeListRow> {
        let (net, buf) = ws.buffer(id)?;
        let server = self.servers.get(&net.id())?;
        let server_row = self.server_tree_row(server)?;
        if buf.is_console() {
            return Some(server_row);
        }
        server_row.set_expanded(true);
        let at = server.children().find(self.buffers.get(&id)?)?;
        server_row.child_row(at)
    }
}

fn server_text(ws: &Workspace, server: ServerId) -> RowText {
    let net = ws.network(server).expect("the network exists");
    RowText {
        label: net.config().name.clone(),
        badge: String::new(),
        classes: owned(classes::server_row_classes(net)),
    }
}

/// Highlights when there are any, else unread, else nothing.
fn badge(counters: textual_core::model::buffer::Counters) -> String {
    match (counters.highlights, counters.unread) {
        (0, 0) => String::new(),
        (0, unread) => unread.to_string(),
        (highlights, _) => highlights.to_string(),
    }
}

fn owned(classes: Vec<&'static str>) -> Vec<String> {
    classes.into_iter().map(str::to_owned).collect()
}

/// A `TreeExpander` around [label, badge]. Bind wires the row's properties
/// to the widgets and the expander's state back to the model.
fn factory(tx: &Inbox) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        let label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let badge = gtk::Label::builder()
            .css_classes([classes::BADGE])
            .visible(false)
            .build();
        let cell = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        cell.append(&label);
        cell.append(&badge);
        let expander = gtk::TreeExpander::new();
        expander.set_child(Some(&cell));
        item.set_child(Some(&expander));
    });
    let bound = Bound::default();
    factory.connect_bind({
        let tx = tx.clone();
        let bound = bound.clone();
        move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
            let (Some(tree_row), Some(expander)) = (
                item.item().and_downcast::<gtk::TreeListRow>(),
                item.child().and_downcast::<gtk::TreeExpander>(),
            ) else {
                return;
            };
            let Some(object) = tree_row.item() else {
                return;
            };
            let cell = expander
                .child()
                .and_downcast::<gtk::Box>()
                .expect("the cell");
            let label = cell.first_child().expect("label");
            let badge = cell.last_child().expect("badge");
            expander.set_list_row(Some(&tree_row));
            let sync = glib::BindingFlags::SYNC_CREATE;
            bound.bind(
                item,
                object
                    .bind_property("label", &label, "label")
                    .flags(sync)
                    .build(),
            );
            bound.bind(
                item,
                object
                    .bind_property("badge", &badge, "label")
                    .flags(sync)
                    .build(),
            );
            bound.bind(
                item,
                object
                    .bind_property("badge", &badge, "visible")
                    .flags(sync)
                    .transform_to(|_, text: String| Some(!text.is_empty()))
                    .build(),
            );
            bound.bind(
                item,
                object
                    .bind_property("classes", &expander, "css-classes")
                    .flags(sync)
                    .build(),
            );
            if let Some(server) = object.downcast_ref::<ServerRow>() {
                let id = server.server();
                let tx = tx.clone();
                let handler = tree_row.connect_expanded_notify(move |row| {
                    let action = UiAction::SetExpanded {
                        server: id,
                        expanded: row.is_expanded(),
                    };
                    let _ = tx.send(action.into());
                });
                bound.keep(item, move || tree_row.disconnect(handler));
            }
        }
    });
    factory.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        bound.release(item);
        if let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() {
            expander.set_list_row(None);
        }
    });
    factory
}
