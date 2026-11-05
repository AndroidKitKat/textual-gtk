//! What the input's keys do with modifiers held: Textual's input key
//! handlers (`TVCMainWindow.m:639-650`, 1044-1064), and AppKit's text view
//! for the keys Textual leaves to it. Command is Super here.

use crate::model::Preferences;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub command: bool,
}

/// What Return or the keypad Enter does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Return {
    Send,
    /// Send the text as an action, as `/me` would.
    SendAsAction,
    /// Leave the key to the text view, which starts a new line.
    Newline,
}

/// `keypad` is the keypad Enter (Textual's `TXKeyEnterCode`), which is
/// the only key Textual gives Control+Enter to.
pub fn on_return(keypad: bool, held: Modifiers, prefs: &Preferences) -> Return {
    let only = |m: Modifiers| held == m;
    if only(Modifiers::default()) {
        Return::Send
    } else if only(Modifiers {
        command: true,
        ..Modifiers::default()
    }) {
        if prefs.command_return_sends_action {
            Return::SendAsAction
        } else {
            Return::Send
        }
    } else if keypad
        && prefs.control_enter_sends_message
        && only(Modifiers {
            control: true,
            ..Modifiers::default()
        })
    {
        Return::Send
    } else {
        Return::Newline
    }
}

/// Whether Up or Down may walk history: Textual registers them bare and
/// with Option (Alt).
pub fn arrows_walk_history(held: Modifiers) -> bool {
    !held.shift && !held.control && !held.command
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers {
        shift: false,
        control: false,
        alt: false,
        command: false,
    };
    const CONTROL: Modifiers = Modifiers {
        control: true,
        ..NONE
    };
    const COMMAND: Modifiers = Modifiers {
        command: true,
        ..NONE
    };
    const ALT: Modifiers = Modifiers { alt: true, ..NONE };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..NONE
    };

    #[test]
    fn return_follows_textuals_defaults() {
        let prefs = Preferences::default();
        for keypad in [false, true] {
            assert_eq!(on_return(keypad, NONE, &prefs), Return::Send);
            assert_eq!(on_return(keypad, COMMAND, &prefs), Return::SendAsAction);
            assert_eq!(on_return(keypad, CONTROL, &prefs), Return::Newline);
            assert_eq!(on_return(keypad, SHIFT, &prefs), Return::Newline);
            assert_eq!(on_return(keypad, ALT, &prefs), Return::Newline);
        }
    }

    #[test]
    fn return_follows_the_preferences() {
        let prefs = Preferences {
            control_enter_sends_message: true,
            command_return_sends_action: false,
            ..Preferences::default()
        };
        assert_eq!(on_return(true, CONTROL, &prefs), Return::Send);
        assert_eq!(on_return(false, CONTROL, &prefs), Return::Newline);
        assert_eq!(on_return(false, COMMAND, &prefs), Return::Send);
        let both = Modifiers {
            control: true,
            command: true,
            ..NONE
        };
        assert_eq!(on_return(true, both, &prefs), Return::Newline);
    }

    #[test]
    fn alt_arrows_walk_history_too() {
        assert!(arrows_walk_history(NONE));
        assert!(arrows_walk_history(ALT));
        assert!(!arrows_walk_history(CONTROL));
        assert!(!arrows_walk_history(SHIFT));
    }
}
