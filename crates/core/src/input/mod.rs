//! The input field's logic, with no model: commands and Textual's typed
//! line rules, splitting a message to fit the wire, history and Tab
//! completion. The workspace holds the state and runs the commands.

pub mod commands;
pub mod completion;
pub mod history;
pub mod split;
