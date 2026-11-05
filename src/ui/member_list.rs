//! The member list of the focused channel: one store the model's
//! positional `MemberDiff`s are spliced into. GTK does no sorting.

use gtk::prelude::*;
use gtk::{gio, glib};
use textual_core::model::buffer::{Body, BufferId};
use textual_core::model::members::MemberDiff;
use textual_core::model::workspace::Workspace;

use crate::ui::classes;
use crate::ui::rows::{Bound, MemberRow};

pub struct MemberList {
    widget: gtk::ScrolledWindow,
    store: gio::ListStore,
    /// The buffer the store mirrors; diffs for any other are ignored.
    shown: Option<BufferId>,
}

/// One row, for the probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberInfo {
    pub nick: String,
    pub prefix: String,
    pub classes: Vec<String>,
}

impl MemberList {
    pub fn build() -> Self {
        let store = gio::ListStore::new::<MemberRow>();
        let selection = gtk::NoSelection::new(Some(store.clone()));
        let list = gtk::ListView::new(Some(selection), Some(factory()));
        list.add_css_class(classes::MEMBER_LIST);
        let widget = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(160)
            .vexpand(true)
            .visible(false)
            .build();
        MemberList {
            widget,
            store,
            shown: None,
        }
    }

    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.widget
    }

    /// Rebuilds the list for a newly focused buffer. A console or query has
    /// no members and hides the list.
    pub fn show(&mut self, ws: &Workspace, id: BufferId) {
        self.shown = Some(id);
        let channel = ws
            .buffer(id)
            .is_some_and(|(_, b)| matches!(b.body(), Body::Channel(_)));
        self.widget.set_visible(channel);
        self.reset(ws, id);
    }

    pub fn apply(&mut self, ws: &Workspace, buffer: BufferId, diff: &MemberDiff) {
        if self.shown != Some(buffer) {
            return;
        }
        match diff {
            MemberDiff::Reset => self.reset(ws, buffer),
            MemberDiff::Insert { at, row } => {
                self.store.insert(*at as u32, &member_row(row));
            }
            MemberDiff::Remove { at } => self.store.remove(*at as u32),
            MemberDiff::Update { at, row } => {
                if let Some(existing) = self.store.item(*at as u32).and_downcast::<MemberRow>() {
                    let (nick, prefix, classes) = member_text(row);
                    existing.update(&nick, &prefix, classes);
                }
            }
            MemberDiff::Move { from, to, row } => {
                self.store.remove(*from as u32);
                self.store.insert(*to as u32, &member_row(row));
            }
        }
    }

    pub fn rows(&self) -> Vec<MemberInfo> {
        if !self.widget.is_visible() {
            return Vec::new();
        }
        (0..self.store.n_items())
            .filter_map(|i| self.store.item(i).and_downcast::<MemberRow>())
            .map(|row| MemberInfo {
                nick: row.nick(),
                prefix: row.prefix(),
                classes: row.classes(),
            })
            .collect()
    }

    fn reset(&mut self, ws: &Workspace, id: BufferId) {
        let rows: Vec<MemberRow> = ws
            .buffer(id)
            .and_then(|(net, buf)| Some((net.session()?, buf.joined()?)))
            .map(|(session, joined)| {
                joined
                    .members()
                    .rows(session.isupport().prefix())
                    .map(|row| member_row(&row))
                    .collect()
            })
            .unwrap_or_default();
        self.store.splice(0, self.store.n_items(), &rows);
    }
}

fn member_text(row: &textual_core::model::members::MemberRow) -> (String, String, Vec<String>) {
    let prefix = row.prefix.map(|(_, symbol)| symbol.to_string());
    (
        row.nick.clone(),
        prefix.unwrap_or_default(),
        classes::member_row_classes(row),
    )
}

fn member_row(row: &textual_core::model::members::MemberRow) -> MemberRow {
    let (nick, prefix, classes) = member_text(row);
    MemberRow::new(&nick, &prefix, classes)
}

/// [prefix, nick], with the row's classes on the cell.
fn factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        let prefix = gtk::Label::builder().width_chars(1).xalign(1.0).build();
        let nick = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let cell = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        cell.append(&prefix);
        cell.append(&nick);
        item.set_child(Some(&cell));
    });
    let bound = Bound::default();
    factory.connect_bind({
        let bound = bound.clone();
        move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
            let (Some(row), Some(cell)) = (
                item.item().and_downcast::<MemberRow>(),
                item.child().and_downcast::<gtk::Box>(),
            ) else {
                return;
            };
            let prefix = cell.first_child().expect("prefix");
            let nick = cell.last_child().expect("nick");
            let sync = glib::BindingFlags::SYNC_CREATE;
            bound.bind(
                item,
                row.bind_property("prefix", &prefix, "label")
                    .flags(sync)
                    .build(),
            );
            bound.bind(
                item,
                row.bind_property("nick", &nick, "label")
                    .flags(sync)
                    .build(),
            );
            bound.bind(
                item,
                row.bind_property("classes", &cell, "css-classes")
                    .flags(sync)
                    .build(),
            );
        }
    });
    factory.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        bound.release(item);
    });
    factory
}
