//! The window: a projection of the `Workspace`. `build` makes the widgets
//! and wires every signal to the inbox; `apply` patches them from one
//! pass's `Change`s and the final workspace. Nothing here reads the model
//! outside `apply`.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use adw::prelude::*;
use gtk::glib;
use textual_core::model::Change;
use textual_core::model::buffer::{Body, BufferId};
use textual_core::model::members::MemberDiff;
use textual_core::model::network::Link;
use textual_core::model::workspace::{Input, UiAction, Workspace};
use textual_core::style::{Style, ViewKind};

use crate::app::{Inbox, Request};
use crate::ui::chat_view::ChatView;
use crate::ui::classes;
use crate::ui::member_list::MemberList;
use crate::ui::sidebar::Sidebar;
use crate::ui::title;

pub struct View {
    window: adw::ApplicationWindow,
    title: adw::WindowTitle,
    sidebar: Sidebar,
    members: MemberList,
    stack: gtk::Stack,
    chats: HashMap<BufferId, ChatView>,
    entry: gtk::Entry,
    style: Rc<Style>,
    pages: PathBuf,
    tx: Inbox,
    /// Shared with the entry's handler, which must not read the model.
    focus: Rc<Cell<Option<BufferId>>>,
}

impl View {
    pub fn build(app: &adw::Application, style: Rc<Style>, tx: Inbox, pages: &Path) -> Self {
        let sidebar = Sidebar::build(&tx);
        let members = MemberList::build();
        let stack = gtk::Stack::builder().hexpand(true).vexpand(true).build();
        let entry = gtk::Entry::builder()
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(6)
            .sensitive(false)
            .build();
        entry.add_css_class(classes::INPUT);

        let chat = gtk::Box::new(gtk::Orientation::Vertical, 0);
        chat.append(&stack);
        chat.append(&entry);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        content.append(&chat);
        content.append(members.widget());

        let split = adw::OverlaySplitView::builder()
            .sidebar(sidebar.widget())
            .content(&content)
            .min_sidebar_width(160.0)
            .max_sidebar_width(260.0)
            .build();
        let title = adw::WindowTitle::new("Textual GTK", "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&split));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Textual GTK")
            .default_width(1000)
            .default_height(640)
            .content(&toolbar)
            .build();
        window.add_css_class(classes::WINDOW);

        window.connect_is_active_notify({
            let tx = tx.clone();
            move |window| {
                let _ = tx.send(UiAction::WindowActive(window.is_active()).into());
            }
        });
        // Closing hides the window and asks the app to quit; the app
        // destroys it once every network has said goodbye.
        window.connect_close_request({
            let tx = tx.clone();
            move |window| {
                if tx.send(Request::Quit).is_err() {
                    return glib::Propagation::Proceed;
                }
                window.set_visible(false);
                glib::Propagation::Stop
            }
        });

        let focus = Rc::new(Cell::new(None::<BufferId>));
        entry.connect_activate({
            let tx = tx.clone();
            let focus = focus.clone();
            move |entry| {
                let text = entry.text().to_string();
                let Some(buffer) = focus.get() else { return };
                if text.is_empty() {
                    return;
                }
                entry.set_text("");
                let _ = tx.send(Input::Say { buffer, text }.into());
            }
        });

        window.present();
        View {
            window,
            title,
            sidebar,
            members,
            stack,
            chats: HashMap::new(),
            entry,
            style,
            pages: pages.to_owned(),
            tx,
            focus,
        }
    }

    pub fn apply(&mut self, ws: &Workspace, changes: &[Change]) {
        // A buffer rebuilt from the final workspace in this pass (its member
        // list reset) must not also take this pass's diffs for it.
        let mut rebuilt = HashSet::new();
        // Only where the focus ends up is selected, once: GTK may also
        // have moved or dropped the selection while rows came and went.
        let restructured = changes
            .iter()
            .any(|c| matches!(c, Change::BufferAdded(_) | Change::BufferRemoved(_)));
        let reselect = restructured
            || changes
                .iter()
                .any(|c| matches!(c, Change::Focus(_) | Change::Expanded(_)));
        if restructured {
            self.sidebar.sync_rows(ws);
        }
        for change in changes {
            match change {
                Change::BufferAdded(_) => {}
                Change::BufferRemoved(id) => {
                    if let Some(chat) = self.chats.remove(id) {
                        self.stack.remove(&chat.widget());
                    }
                }
                Change::BufferChanged(id) => {
                    self.sidebar.refresh_buffer(ws, *id);
                    if let Some(chat) = self.chats.get(id) {
                        chat.set_topic(&topic_of(ws, *id));
                    }
                }
                Change::Counters(id) => self.sidebar.refresh_buffer(ws, *id),
                Change::Line { buffer, line } => {
                    if let Some(chat) = self.chats.get(buffer) {
                        chat.append(line.render(&self.style), &line.id.to_string());
                    }
                }
                Change::Members { buffer, diff } => {
                    if rebuilt.contains(buffer) {
                        continue;
                    }
                    if *diff == MemberDiff::Reset {
                        rebuilt.insert(*buffer);
                    }
                    self.members.apply(ws, *buffer, diff);
                }
                Change::Focus(id) => {
                    self.focus.set(Some(*id));
                    self.show_chat(ws, *id);
                    self.members.show(ws, *id);
                    rebuilt.insert(*id);
                }
                Change::Expanded(server) => {
                    self.sidebar.set_expanded(*server, ws.is_expanded(*server));
                }
                Change::Link(server) => self.sidebar.refresh_server(ws, *server),
            }
        }
        if reselect && let Some(id) = ws.focus() {
            self.sidebar.select(ws, id);
        }
        for (id, chat) in &self.chats {
            if chat.needs_replay() {
                let lines = ws
                    .buffer(*id)
                    .map(|(_, b)| b.scrollback().lines().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                chat.fill(
                    &topic_of(ws, *id),
                    lines
                        .iter()
                        .map(|l| (l.render(&self.style), l.id.to_string())),
                );
            }
        }
        self.refresh_chrome(ws);
    }

    /// Creates the buffer's chat view on first focus and raises it. The
    /// page fills itself from the scrollback once it has loaded.
    fn show_chat(&mut self, ws: &Workspace, id: BufferId) {
        if !self.chats.contains_key(&id) {
            let Some((_, buf)) = ws.buffer(id) else {
                return;
            };
            let first = self.chats.values().next().map(ChatView::widget);
            let tx = self.tx.clone();
            let kind = match buf.body() {
                Body::Console => ViewKind::Server,
                Body::Channel(_) => ViewKind::Channel(buf.name()),
                Body::Query => ViewKind::Query(buf.name()),
            };
            let chat = ChatView::new(
                self.style.clone(),
                kind,
                &self.pages,
                first.as_ref(),
                move || {
                    let _ = tx.send(Input::Wake.into());
                },
            );
            self.stack.add_named(&chat.widget(), Some(&id.to_string()));
            self.chats.insert(id, chat);
        }
        self.stack.set_visible_child_name(&id.to_string());
    }

    fn refresh_chrome(&self, ws: &Workspace) {
        let (title, subtitle) = title::title(ws, Instant::now());
        if self.title.title() != title {
            self.title.set_title(&title);
        }
        if self.title.subtitle() != subtitle {
            self.title.set_subtitle(&subtitle);
        }
        let online = ws
            .focus()
            .and_then(|id| ws.buffer(id))
            .is_some_and(|(net, _)| matches!(net.link(), Link::Online { .. }));
        if self.entry.is_sensitive() != online {
            self.entry.set_sensitive(online);
        }
    }

    pub fn focused_chat(&self) -> Option<&ChatView> {
        self.focus.get().and_then(|id| self.chats.get(&id))
    }

    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.window
    }

    pub fn title_widget(&self) -> &adw::WindowTitle {
        &self.title
    }

    pub fn entry(&self) -> &gtk::Entry {
        &self.entry
    }

    pub fn sidebar(&self) -> &Sidebar {
        &self.sidebar
    }

    pub fn members(&self) -> &MemberList {
        &self.members
    }
}

fn topic_of(ws: &Workspace, id: BufferId) -> String {
    ws.buffer(id)
        .and_then(|(_, b)| b.joined())
        .and_then(|j| j.topic())
        .unwrap_or_default()
        .to_owned()
}
