//! The input field: a multi-line text view that grows with its text, as
//! Textual's `TVCMainWindowTextView`. Keys only become `UiAction`s; what
//! they do is decided in the model, which answers with `UiEffect`s.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib};
use textual_core::model::buffer::BufferId;
use textual_core::model::workspace::{UiAction, UiEffect};

use crate::app::Inbox;
use crate::ui::classes;

/// About five lines; past that the field scrolls.
const MAX_HEIGHT: i32 = 110;

/// A key the field acts on. The window's and the field's key controllers
/// map events to these, and the probe presses them directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Return,
    Tab,
    ShiftTab,
    /// Up and Down, which walk history only from the first or last line.
    Up,
    Down,
    /// Ctrl+P and Ctrl+N, which always walk history.
    Previous,
    Next,
}

impl Key {
    pub fn parse(name: &str) -> Option<Key> {
        Some(match name {
            "Return" => Key::Return,
            "Tab" => Key::Tab,
            "ShiftTab" => Key::ShiftTab,
            "Up" => Key::Up,
            "Down" => Key::Down,
            "CtrlP" => Key::Previous,
            "CtrlN" => Key::Next,
            _ => return None,
        })
    }
}

#[derive(Clone)]
pub struct InputBar {
    scroller: gtk::ScrolledWindow,
    view: gtk::TextView,
    tx: Inbox,
    /// The focused buffer, set by the view from the model's `Focus`.
    focus: Rc<Cell<Option<BufferId>>>,
    /// The open flood question and the text it asks about.
    flood: Rc<RefCell<Option<(adw::AlertDialog, BufferId, String)>>>,
}

impl InputBar {
    pub fn build(tx: Inbox, focus: Rc<Cell<Option<BufferId>>>) -> Self {
        let view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .accepts_tab(false)
            .top_margin(6)
            .bottom_margin(6)
            .left_margin(8)
            .right_margin(8)
            .build();
        view.add_css_class(classes::INPUT);
        let scroller = gtk::ScrolledWindow::builder()
            .child(&view)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(MAX_HEIGHT)
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        let bar = InputBar {
            scroller,
            view,
            tx,
            focus,
            flood: Rc::default(),
        };
        bar.view.buffer().connect_changed({
            let tx = bar.tx.clone();
            move |buffer| {
                let _ = tx.send(UiAction::Draft(text_of(buffer)).into());
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed({
            let bar = bar.clone();
            move |_, key, _, state| {
                let plain = !state.intersects(
                    gdk::ModifierType::SHIFT_MASK
                        | gdk::ModifierType::CONTROL_MASK
                        | gdk::ModifierType::ALT_MASK,
                );
                let key = match key {
                    gdk::Key::Return | gdk::Key::KP_Enter if plain => Key::Return,
                    gdk::Key::Up if plain => Key::Up,
                    gdk::Key::Down if plain => Key::Down,
                    _ => return glib::Propagation::Proceed,
                };
                if bar.press(key) {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        });
        bar.view.add_controller(keys);
        bar
    }

    /// Tab, Shift+Tab, Ctrl+P and Ctrl+N work wherever the focus is in the
    /// window, as Textual registers them on the window.
    pub fn install_window_keys(&self, window: &adw::ApplicationWindow) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let bar = self.clone();
            move |_, key, _, state| {
                if bar.flood.borrow().is_some() {
                    return glib::Propagation::Proceed;
                }
                let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
                let key = match key {
                    gdk::Key::Tab if !ctrl => Key::Tab,
                    gdk::Key::ISO_Left_Tab if !ctrl => Key::ShiftTab,
                    gdk::Key::p if ctrl => Key::Previous,
                    gdk::Key::n if ctrl => Key::Next,
                    _ => return glib::Propagation::Proceed,
                };
                bar.press(key);
                glib::Propagation::Stop
            }
        });
        window.add_controller(keys);
    }

    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.scroller
    }

    pub fn text(&self) -> String {
        text_of(&self.view.buffer())
    }

    /// The caret, in characters.
    pub fn caret(&self) -> usize {
        let buffer = self.view.buffer();
        buffer.iter_at_mark(&buffer.get_insert()).offset() as usize
    }

    /// Replaces the text and puts the caret at `caret` characters.
    pub fn set(&self, text: &str, caret: usize) {
        let buffer = self.view.buffer();
        buffer.set_text(text);
        let at = buffer.iter_at_offset(caret.min(i32::MAX as usize) as i32);
        buffer.place_cursor(&at);
    }

    /// Acts on a key. `false` leaves it to the text view: Up and Down that
    /// only move the caret between lines.
    pub fn press(&self, key: Key) -> bool {
        let text = self.text();
        let action = match key {
            Key::Return => {
                let Some(buffer) = self.focus.get() else {
                    return true;
                };
                self.view.buffer().set_text("");
                UiAction::Submit {
                    buffer,
                    text,
                    confirmed: false,
                }
            }
            Key::Tab | Key::ShiftTab => {
                self.view.grab_focus();
                let buffer = self.view.buffer();
                let (start, end) = buffer
                    .selection_bounds()
                    .map(|(s, e)| (s.offset() as usize, e.offset() as usize))
                    .unwrap_or_else(|| (self.caret(), self.caret()));
                UiAction::Complete {
                    text,
                    selection: (start, end),
                    backward: key == Key::ShiftTab,
                }
            }
            Key::Up | Key::Down if !self.on_edge_line(key == Key::Up) => return false,
            Key::Up | Key::Previous => UiAction::History { text, older: true },
            Key::Down | Key::Next => UiAction::History { text, older: false },
        };
        let _ = self.tx.send(action.into());
        true
    }

    /// Whether the caret is on the first (or last) line as laid out, where
    /// Textual's arrows leave the text for history.
    fn on_edge_line(&self, first: bool) -> bool {
        let buffer = self.view.buffer();
        let mut at = buffer.iter_at_mark(&buffer.get_insert());
        if first {
            !self.view.backward_display_line(&mut at)
        } else {
            !self.view.forward_display_line(&mut at)
        }
    }

    /// Carries out what the model asked of the field.
    pub fn apply(&self, effect: &UiEffect, window: &adw::ApplicationWindow) {
        match effect {
            UiEffect::SetInput { was, text, caret } => {
                if self.text() == *was {
                    self.set(text, *caret);
                    self.view.grab_focus();
                }
            }
            UiEffect::ConfirmFlood { buffer, text } => self.ask_flood(*buffer, text, window),
        }
    }

    /// Textual's possible-flood alert (`IRC[u4c-7i]`, `IRC[lql-8i]`).
    fn ask_flood(&self, buffer: BufferId, text: &str, window: &adw::ApplicationWindow) {
        let dialog = adw::AlertDialog::new(
            Some(
                "The message that you are sending is very large which means it will be split \
                 up into multiple messages. Sending too many messages at once is often \
                 considered disruptive and may result in a ban.",
            ),
            Some("Are you sure you want to send this message?"),
        );
        dialog.add_responses(&[("no", "No"), ("yes", "Yes")]);
        dialog.set_default_response(Some("yes"));
        dialog.set_close_response("no");
        dialog.add_css_class(classes::FLOOD_QUESTION);
        dialog.connect_response(None, {
            let bar = self.clone();
            move |_, response| bar.answer_flood(response == "yes")
        });
        *self.flood.borrow_mut() = Some((dialog.clone(), buffer, text.to_owned()));
        dialog.present(Some(window));
    }

    /// Answers the open flood question, from its buttons or the probe.
    pub fn answer_flood(&self, send: bool) {
        let Some((dialog, buffer, text)) = self.flood.borrow_mut().take() else {
            return;
        };
        dialog.force_close();
        if send {
            let action = UiAction::Submit {
                buffer,
                text,
                confirmed: true,
            };
            let _ = self.tx.send(action.into());
        }
    }

    /// The text the open flood question is about.
    pub fn flood_question(&self) -> Option<String> {
        self.flood
            .borrow()
            .as_ref()
            .map(|(_, _, text)| text.clone())
    }
}

fn text_of(buffer: &gtk::TextBuffer) -> String {
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .to_string()
}
