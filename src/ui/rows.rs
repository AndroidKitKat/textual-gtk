//! The GObjects behind the sidebar and member list rows. Each carries the
//! strings a widget binds to; the view updates them in place, only when a
//! value changed, so GTK keeps the row's selection and expansion.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use textual_core::model::buffer::BufferId;
use textual_core::model::network::ServerId;

/// Each `Properties` derive needs a module of its own.
mod server_imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::ServerRow)]
    pub struct ServerRow {
        #[property(get, set)]
        pub label: RefCell<String>,
        #[property(get, set)]
        pub badge: RefCell<String>,
        #[property(get, set)]
        pub classes: RefCell<Vec<String>>,
        pub server: Cell<Option<ServerId>>,
        pub console: Cell<Option<BufferId>>,
        pub children: RefCell<Option<gtk::gio::ListStore>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ServerRow {
        const NAME: &'static str = "TxServerRow";
        type Type = super::ServerRow;
    }

    #[glib::derived_properties]
    impl ObjectImpl for ServerRow {}
}

mod buffer_imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::BufferRow)]
    pub struct BufferRow {
        #[property(get, set)]
        pub label: RefCell<String>,
        #[property(get, set)]
        pub badge: RefCell<String>,
        #[property(get, set)]
        pub classes: RefCell<Vec<String>>,
        pub id: Cell<Option<BufferId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BufferRow {
        const NAME: &'static str = "TxBufferRow";
        type Type = super::BufferRow;
    }

    #[glib::derived_properties]
    impl ObjectImpl for BufferRow {}
}

mod member_imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::MemberRow)]
    pub struct MemberRow {
        #[property(get, set)]
        pub nick: RefCell<String>,
        #[property(get, set)]
        pub prefix: RefCell<String>,
        #[property(get, set)]
        pub classes: RefCell<Vec<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MemberRow {
        const NAME: &'static str = "TxMemberRow";
        type Type = super::MemberRow;
    }

    #[glib::derived_properties]
    impl ObjectImpl for MemberRow {}
}

glib::wrapper! {
    pub struct ServerRow(ObjectSubclass<server_imp::ServerRow>);
}

glib::wrapper! {
    pub struct BufferRow(ObjectSubclass<buffer_imp::BufferRow>);
}

glib::wrapper! {
    pub struct MemberRow(ObjectSubclass<member_imp::MemberRow>);
}

/// What a sidebar row shows. Both row kinds take the same three strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowText {
    pub label: String,
    pub badge: String,
    pub classes: Vec<String>,
}

impl ServerRow {
    pub fn new(server: ServerId, console: BufferId, text: &RowText) -> Self {
        let row: Self = glib::Object::new();
        let imp = row.imp();
        imp.server.set(Some(server));
        imp.console.set(Some(console));
        imp.children
            .replace(Some(gtk::gio::ListStore::new::<BufferRow>()));
        row.update(text);
        row
    }

    pub fn server(&self) -> ServerId {
        self.imp().server.get().expect("set at construction")
    }

    pub fn console(&self) -> BufferId {
        self.imp().console.get().expect("set at construction")
    }

    pub fn children(&self) -> gtk::gio::ListStore {
        self.imp()
            .children
            .borrow()
            .clone()
            .expect("set at construction")
    }

    pub fn update(&self, text: &RowText) {
        if self.label() != text.label {
            self.set_label(text.label.as_str());
        }
        if self.badge() != text.badge {
            self.set_badge(text.badge.as_str());
        }
        if self.classes() != text.classes {
            self.set_classes(text.classes.clone());
        }
    }
}

impl BufferRow {
    pub fn new(id: BufferId, text: &RowText) -> Self {
        let row: Self = glib::Object::new();
        row.imp().id.set(Some(id));
        row.update(text);
        row
    }

    pub fn id(&self) -> BufferId {
        self.imp().id.get().expect("set at construction")
    }

    pub fn update(&self, text: &RowText) {
        if self.label() != text.label {
            self.set_label(text.label.as_str());
        }
        if self.badge() != text.badge {
            self.set_badge(text.badge.as_str());
        }
        if self.classes() != text.classes {
            self.set_classes(text.classes.clone());
        }
    }
}

impl MemberRow {
    pub fn new(nick: &str, prefix: &str, classes: Vec<String>) -> Self {
        let row: Self = glib::Object::new();
        row.update(nick, prefix, classes);
        row
    }

    pub fn update(&self, nick: &str, prefix: &str, classes: Vec<String>) {
        if self.nick() != nick {
            self.set_nick(nick);
        }
        if self.prefix() != prefix {
            self.set_prefix(prefix);
        }
        if self.classes() != classes {
            self.set_classes(classes);
        }
    }
}

/// Which buffer a sidebar row stands for: the console for a server row.
pub fn buffer_of(item: &glib::Object) -> Option<BufferId> {
    if let Some(row) = item.downcast_ref::<BufferRow>() {
        Some(row.id())
    } else {
        item.downcast_ref::<ServerRow>().map(ServerRow::console)
    }
}

/// Per list item, what to undo when it is unbound: property bindings and
/// signal handlers made while binding. Keyed by the item's address because
/// a recycled item is bound again to another row.
#[derive(Default, Clone)]
pub struct Bound(Rc<RefCell<HashMap<usize, Vec<Undo>>>>);

type Undo = Box<dyn FnOnce()>;

impl Bound {
    pub fn keep(&self, item: &gtk::ListItem, undo: impl FnOnce() + 'static) {
        self.0
            .borrow_mut()
            .entry(item.as_ptr() as usize)
            .or_default()
            .push(Box::new(undo));
    }

    pub fn bind(&self, item: &gtk::ListItem, binding: glib::Binding) {
        self.keep(item, move || binding.unbind());
    }

    pub fn release(&self, item: &gtk::ListItem) {
        let undo = self.0.borrow_mut().remove(&(item.as_ptr() as usize));
        for f in undo.unwrap_or_default() {
            f();
        }
    }
}
