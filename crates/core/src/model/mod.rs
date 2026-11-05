pub mod buffer;
pub mod members;
pub mod network;
pub mod registration;
pub mod session;
pub mod workspace;

use std::rc::Rc;
use std::time::Instant;

use chrono::{DateTime, Local};

use crate::line::{Line, LineId, Stamp};
use buffer::BufferId;
use members::MemberDiff;
use network::ServerId;

/// The time, read once per pass by the app: `mono` drives timers, `wall`
/// stamps printed lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Now {
    pub mono: Instant,
    pub wall: DateTime<Local>,
}

/// Textual's application-wide preferences that the model follows. They
/// are global in Textual (`TPCPreferences`), not per server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Preferences {
    /// `RejoinChannelOnLocalKick`, off by default: join a channel again
    /// three seconds after being kicked from it.
    pub rejoin_on_kick: bool,
}

/// A model fact the view reacts to. Most carry only an id and the view
/// reads the rest from the workspace; a line and a member diff carry their
/// payload because later changes in the same pass can make them
/// unreadable.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    BufferAdded(BufferId),
    BufferRemoved(BufferId),
    /// Name, joined state, topic or modes.
    BufferChanged(BufferId),
    Line {
        buffer: BufferId,
        line: Rc<Line>,
    },
    Counters(BufferId),
    Members {
        buffer: BufferId,
        diff: MemberDiff,
    },
    Focus(BufferId),
    Expanded(ServerId),
    /// The network's `Link` moved to another variant.
    Link(ServerId),
}

/// Ids handed out by one workspace, in order.
#[derive(Debug, Default)]
pub(crate) struct Ids {
    next_line: u64,
    next_buffer: u64,
}

impl Ids {
    pub(crate) fn buffer(&mut self) -> BufferId {
        self.next_buffer += 1;
        BufferId::new(self.next_buffer)
    }

    fn line(&mut self) -> LineId {
        self.next_line += 1;
        LineId::new(self.next_line)
    }
}

/// What a network needs from the workspace for one input.
pub(crate) struct Cx<'a> {
    pub now: Now,
    pub ids: &'a mut Ids,
    pub prefs: Preferences,
}

impl Cx<'_> {
    pub(crate) fn stamp(&mut self) -> Stamp {
        Stamp {
            id: self.ids.line(),
            at: self.now.wall,
        }
    }
}
