//! What was typed before, walked with Up and Down: Textual's
//! `TLOInputHistoryObject` (`TLOInputHistory.m:269-388`), including the
//! way it keeps a draft. Up or Down on a field that differs from the entry
//! at the current position first appends the field to the history, so an
//! unsent line is never lost: Up recalls older lines and Down comes back to
//! the draft.

/// Textual's `_inputHistoryMax`.
pub const MAX_ENTRIES: usize = 100;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    /// Oldest first.
    entries: Vec<String>,
    /// Textual's `historyBufferPosition`; `entries.len()` is past the newest.
    position: usize,
}

impl History {
    /// A sent line. Textual's repeat check (`TLOInputHistory.m:299`,
    /// `lastEntry != nil || ...`) is always true, so it keeps every repeat;
    /// a line equal to the newest entry is not added again here.
    pub fn add(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.entries.last().map(String::as_str) != Some(text) {
            self.push(text);
        }
        self.position = self.entries.len();
    }

    /// The text for the field after Up, or `None` to leave it alone.
    pub fn up(&mut self, field: &str) -> Option<String> {
        if !field.is_empty() && self.current() != Some(field) {
            self.push(field);
        }
        if self.position == 0 {
            return None;
        }
        self.position -= 1;
        self.entries.get(self.position).cloned()
    }

    /// The text for the field after Down, or `None` to leave it alone. A
    /// field that is not the current entry is kept as a new entry and
    /// cleared.
    pub fn down(&mut self, field: &str) -> Option<String> {
        if field.is_empty() {
            self.position = self.entries.len();
            return None;
        }
        if self.current() != Some(field) {
            self.push(field);
            return Some(String::new());
        }
        self.position += 1;
        Some(self.current().unwrap_or_default().to_owned())
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    fn current(&self) -> Option<&str> {
        self.entries.get(self.position).map(String::as_str)
    }

    /// Appends without moving the position, dropping the oldest entry past
    /// the cap. Like Textual, a drop does not shift the position.
    fn push(&mut self, text: &str) {
        self.entries.push(text.to_owned());
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(lines: &[&str]) -> History {
        let mut h = History::default();
        for l in lines {
            h.add(l);
        }
        h
    }

    #[test]
    fn up_walks_back_and_stops_at_the_oldest() {
        let mut h = with(&["a", "b"]);
        assert_eq!(h.up("").as_deref(), Some("b"));
        assert_eq!(h.up("b").as_deref(), Some("a"));
        assert_eq!(h.up("a"), None);
        assert_eq!(h.up("a"), None);
        assert_eq!(h.entries(), ["a", "b"]);
    }

    #[test]
    fn down_walks_forward_then_clears() {
        let mut h = with(&["a", "b"]);
        h.up("");
        h.up("b");
        assert_eq!(h.down("a").as_deref(), Some("b"));
        assert_eq!(h.down("b").as_deref(), Some(""));
        assert_eq!(h.down(""), None);
        assert_eq!(h.up("").as_deref(), Some("b"));
    }

    #[test]
    fn a_draft_is_kept_by_up_and_found_again_by_down() {
        let mut h = with(&["a", "b"]);
        assert_eq!(h.up("draft").as_deref(), Some("b"));
        assert_eq!(h.entries(), ["a", "b", "draft"]);
        assert_eq!(h.down("b").as_deref(), Some("draft"));
        assert_eq!(h.down("draft").as_deref(), Some(""));
    }

    #[test]
    fn down_on_unsent_text_keeps_it_and_clears_the_field() {
        let mut h = with(&["a"]);
        assert_eq!(h.down("typed").as_deref(), Some(""));
        assert_eq!(h.entries(), ["a", "typed"]);
        // The position stayed past "a", which "typed" now occupies, so Up
        // goes to "a" and Down from there finds the kept text.
        assert_eq!(h.up("").as_deref(), Some("a"));
        assert_eq!(h.down("a").as_deref(), Some("typed"));
    }

    #[test]
    fn an_edited_recall_is_kept_as_a_new_entry() {
        let mut h = with(&["a", "b"]);
        h.up("");
        assert_eq!(h.up("b edited").as_deref(), Some("a"));
        assert_eq!(h.entries(), ["a", "b", "b edited"]);
    }

    #[test]
    fn up_on_an_empty_history() {
        let mut h = History::default();
        assert_eq!(h.up(""), None);
        assert_eq!(h.down(""), None);
        assert_eq!(h.up("x"), None);
        assert_eq!(h.entries(), ["x"]);
    }

    #[test]
    fn a_repeat_of_the_newest_line_is_added_once_and_empty_never() {
        let mut h = with(&["a", "a", "", "b", "a"]);
        assert_eq!(h.entries(), ["a", "b", "a"]);
        h.add("a");
        assert_eq!(h.entries(), ["a", "b", "a"]);
        assert_eq!(h.up("").as_deref(), Some("a"));
    }

    #[test]
    fn the_oldest_entry_goes_past_the_cap() {
        let mut h = History::default();
        for i in 0..=MAX_ENTRIES {
            h.add(&i.to_string());
        }
        assert_eq!(h.entries().len(), MAX_ENTRIES);
        assert_eq!(h.entries()[0], "1");
        assert_eq!(h.up("").as_deref(), Some("100"));
    }
}
