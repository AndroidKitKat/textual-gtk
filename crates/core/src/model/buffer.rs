//! The buffers of one network in sidebar order (console, channels, then
//! queries), with each buffer's scrollback and unread counters.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::rc::Rc;
use std::time::Instant;

use crate::irc::casemap::CaseMapping;
use crate::line::{Line, LineType};
use crate::model::Change;
use crate::model::members::{Member, MemberDiff, Members};

/// Lines kept per buffer.
pub const SCROLLBACK: usize = 1000;

/// Stable identity of a buffer. Handed out in order by the workspace for
/// now; unit 8 gives configured channels their config UUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BufferId(u64);

impl BufferId {
    pub const fn new(n: u64) -> Self {
        BufferId(n)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for BufferId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "b{}", self.0)
    }
}

#[derive(Debug)]
pub struct Buffer {
    id: BufferId,
    /// The channel name or the query's nick, as the server last spelled
    /// it; the server's address for the console.
    name: String,
    body: Body,
    counters: Counters,
    scrollback: Scrollback,
}

#[derive(Debug)]
pub enum Body {
    Console,
    Channel(ChannelState),
    Query,
}

/// A parted channel has no members, topic or modes, so leaving one cannot
/// leave any of them behind.
#[derive(Debug)]
pub enum ChannelState {
    /// Not in the channel. `rejoin` says whether and when we JOIN it again.
    Parted {
        rejoin: Rejoin,
    },
    Joined(Joined),
}

/// Why a parted channel will be joined. Configured autojoin channels and
/// channels we were in when the link ended wait for 001; a KICK waits 3 s
/// (Textual's `joinKickedChannel` after 3.0 s, IRCClient.m:7348).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejoin {
    /// We left on purpose. Only a configured autojoin channel is joined
    /// again, as Textual's `performAutoJoin` does for every inactive one.
    No,
    OnConnect,
    At(Instant),
}

#[derive(Debug)]
pub struct Joined {
    pub(crate) topic: Option<String>,
    /// Channel modes other than list and prefix modes, with their
    /// parameter if they take one.
    pub(crate) modes: BTreeMap<char, Option<String>>,
    pub(crate) members: Members,
    /// 353 entries collected until 366 swaps them in.
    pub(crate) names: Option<Vec<Member>>,
    pub(crate) sync: Sync,
}

/// How far the join's first member sync has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sync {
    Names,
    /// Our hidden WHO is out; its replies are not printed.
    Who,
    Done,
}

impl Joined {
    fn new() -> Self {
        Joined {
            topic: None,
            modes: BTreeMap::new(),
            members: Members::default(),
            names: None,
            sync: Sync::Names,
        }
    }

    pub fn topic(&self) -> Option<&str> {
        self.topic.as_deref()
    }

    pub fn members(&self) -> &Members {
        &self.members
    }

    /// `+modes params`, as a 324 reply shows them.
    pub fn modes(&self) -> String {
        let letters: String = self.modes.keys().collect();
        let params = self.modes.values().flatten();
        std::iter::once(format!("+{letters}"))
            .chain(params.cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Unread messages and highlights since the user last looked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub unread: u32,
    pub highlights: u32,
}

#[derive(Debug, Default)]
pub struct Scrollback {
    lines: VecDeque<Rc<Line>>,
}

impl Scrollback {
    pub fn lines(&self) -> impl DoubleEndedIterator<Item = &Rc<Line>> {
        self.lines.iter()
    }

    pub fn last(&self) -> Option<&Rc<Line>> {
        self.lines.back()
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    fn has_msgid(&self, msgid: &str) -> bool {
        self.lines.iter().any(|l| l.msgid.as_deref() == Some(msgid))
    }

    fn push(&mut self, line: Rc<Line>) {
        if self.lines.len() == SCROLLBACK {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }
}

impl Buffer {
    fn new(id: BufferId, name: &str, body: Body) -> Self {
        Buffer {
            id,
            name: name.to_owned(),
            body,
            counters: Counters::default(),
            scrollback: Scrollback::default(),
        }
    }

    pub fn id(&self) -> BufferId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn body(&self) -> &Body {
        &self.body
    }

    pub fn is_console(&self) -> bool {
        matches!(self.body, Body::Console)
    }

    pub fn joined(&self) -> Option<&Joined> {
        match &self.body {
            Body::Channel(ChannelState::Joined(joined)) => Some(joined),
            _ => None,
        }
    }

    /// `None` unless this is a parted channel.
    pub fn rejoin(&self) -> Option<Rejoin> {
        match self.body {
            Body::Channel(ChannelState::Parted { rejoin }) => Some(rejoin),
            _ => None,
        }
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    pub fn scrollback(&self) -> &Scrollback {
        &self.scrollback
    }

    /// Counts a line printed where the user is not looking, as Textual
    /// does: only what someone else said, never in the console, and a
    /// channel NOTICE not at all (`IRCClient _receiveText_Public` only
    /// notifies for one). Returns whether the counters changed.
    pub(crate) fn count(&mut self, line: &Line) -> bool {
        let channel_notice = matches!(self.body, Body::Channel(_)) && line.kind == LineType::Notice;
        if self.is_console() || !line.kind.is_spoken() || line.from_self || channel_notice {
            return false;
        }
        self.counters.unread += 1;
        self.counters.highlights += u32::from(line.highlight);
        true
    }

    /// Returns whether there was anything to clear.
    pub(crate) fn mark_read(&mut self) -> bool {
        std::mem::take(&mut self.counters) != Counters::default()
    }

    fn joined_mut(&mut self) -> Option<&mut Joined> {
        match &mut self.body {
            Body::Channel(ChannelState::Joined(joined)) => Some(joined),
            _ => None,
        }
    }

    fn rejoin_mut(&mut self) -> Option<&mut Rejoin> {
        match &mut self.body {
            Body::Channel(ChannelState::Parted { rejoin }) => Some(rejoin),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Buffers {
    /// Console first, then channels, then queries.
    list: Vec<Buffer>,
}

impl Buffers {
    pub(crate) fn new(console: BufferId, name: &str) -> Self {
        Buffers {
            list: vec![Buffer::new(console, name, Body::Console)],
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Buffer> {
        self.list.iter()
    }

    pub fn get(&self, id: BufferId) -> Option<&Buffer> {
        self.list.iter().find(|b| b.id == id)
    }

    pub fn console(&self) -> &Buffer {
        &self.list[0]
    }

    pub(crate) fn get_mut(&mut self, id: BufferId) -> Option<&mut Buffer> {
        self.list.iter_mut().find(|b| b.id == id)
    }

    /// The channel buffer for `name`, joined or not.
    pub(crate) fn find_channel(&self, casemap: CaseMapping, name: &str) -> Option<BufferId> {
        let key = casemap.chan(name);
        self.list
            .iter()
            .find(|b| matches!(b.body, Body::Channel(_)) && casemap.chan(&b.name) == key)
            .map(|b| b.id)
    }

    pub(crate) fn find_joined(&self, casemap: CaseMapping, name: &str) -> Option<BufferId> {
        self.find_channel(casemap, name)
            .filter(|id| self.get(*id).is_some_and(|b| b.joined().is_some()))
    }

    pub(crate) fn find_query(&self, casemap: CaseMapping, nick: &str) -> Option<BufferId> {
        let key = casemap.nick(nick);
        self.list
            .iter()
            .find(|b| matches!(b.body, Body::Query) && casemap.nick(&b.name) == key)
            .map(|b| b.id)
    }

    pub(crate) fn joined_mut(&mut self, id: BufferId) -> Option<&mut Joined> {
        self.get_mut(id).and_then(Buffer::joined_mut)
    }

    /// Every joined channel, for QUIT, NICK, AWAY and the like.
    pub(crate) fn all_joined_mut(&mut self) -> impl Iterator<Item = (BufferId, &mut Joined)> {
        self.list
            .iter_mut()
            .filter_map(|b| Some(b.id).zip(b.joined_mut()))
    }

    /// Adds a parted channel after the other channels.
    pub(crate) fn add_channel(
        &mut self,
        id: BufferId,
        name: &str,
        rejoin: Rejoin,
        changes: &mut Vec<Change>,
    ) -> BufferId {
        let at = 1 + self
            .list
            .iter()
            .filter(|b| matches!(b.body, Body::Channel(_)))
            .count();
        let body = Body::Channel(ChannelState::Parted { rejoin });
        self.list.insert(at, Buffer::new(id, name, body));
        changes.push(Change::BufferAdded(id));
        id
    }

    pub(crate) fn add_query(
        &mut self,
        id: BufferId,
        nick: &str,
        changes: &mut Vec<Change>,
    ) -> BufferId {
        self.list.push(Buffer::new(id, nick, Body::Query));
        changes.push(Change::BufferAdded(id));
        id
    }

    pub(crate) fn remove(&mut self, id: BufferId, changes: &mut Vec<Change>) {
        if let Some(at) = self.list.iter().position(|b| b.id == id && !b.is_console()) {
            self.list.remove(at);
            changes.push(Change::BufferRemoved(id));
        }
    }

    pub(crate) fn rename(&mut self, id: BufferId, name: &str, changes: &mut Vec<Change>) {
        if let Some(b) = self.get_mut(id)
            && b.name != name
        {
            name.clone_into(&mut b.name);
            changes.push(Change::BufferChanged(id));
        }
    }

    /// Marks a channel joined. `false` if it already was.
    pub(crate) fn join(&mut self, id: BufferId, changes: &mut Vec<Change>) -> bool {
        match self.get_mut(id).map(|b| &mut b.body) {
            Some(Body::Channel(state @ ChannelState::Parted { .. })) => {
                *state = ChannelState::Joined(Joined::new());
                changes.push(Change::BufferChanged(id));
                true
            }
            _ => false,
        }
    }

    pub(crate) fn part(&mut self, id: BufferId, rejoin: Rejoin, changes: &mut Vec<Change>) {
        if let Some(Body::Channel(state @ ChannelState::Joined(_))) =
            self.get_mut(id).map(|b| &mut b.body)
        {
            *state = ChannelState::Parted { rejoin };
            changes.push(Change::Members {
                buffer: id,
                diff: MemberDiff::Reset,
            });
            changes.push(Change::BufferChanged(id));
        }
    }

    /// The link ended: every channel is parted, and each one that was open
    /// or waiting to be rejoined is joined again at the next 001.
    pub(crate) fn part_all(&mut self, changes: &mut Vec<Change>) {
        let joined: Vec<BufferId> = self
            .list
            .iter()
            .filter(|b| b.joined().is_some())
            .map(|b| b.id)
            .collect();
        for id in joined {
            self.part(id, Rejoin::OnConnect, changes);
        }
        for rejoin in self.list.iter_mut().filter_map(Buffer::rejoin_mut) {
            if matches!(rejoin, Rejoin::At(_)) {
                *rejoin = Rejoin::OnConnect;
            }
        }
    }

    /// When the first kicked channel is due to be joined again.
    pub(crate) fn next_rejoin(&self) -> Option<Instant> {
        self.list
            .iter()
            .filter_map(|b| match b.rejoin() {
                Some(Rejoin::At(at)) => Some(at),
                _ => None,
            })
            .min()
    }

    /// The kicked channels whose wait is over. They move to `OnConnect`,
    /// so a link that drops before the server's JOIN still rejoins them
    /// and a second wake does not send another JOIN.
    pub(crate) fn take_due_rejoins(&mut self, now: Instant) -> Vec<BufferId> {
        let mut due = Vec::new();
        for b in &mut self.list {
            let id = b.id;
            if let Some(rejoin) = b.rejoin_mut()
                && matches!(rejoin, Rejoin::At(at) if *at <= now)
            {
                *rejoin = Rejoin::OnConnect;
                due.push(id);
            }
        }
        due
    }

    /// The end of the print funnel: keeps the line unless this buffer
    /// already has its `msgid` (a server delivering a message to our own
    /// nick sends it twice).
    /// Textual's conversation tracking for a spoken line in a joined
    /// channel. A line `received` from the server first weighs its sender
    /// (`IRCClient.m:6510-6521`: toward us when the text holds our nick
    /// without its underscores); then every member the line names is
    /// weighed, toward us when the line is ours (`TVCLogController.m:1289`).
    pub(crate) fn converse(
        &mut self,
        id: BufferId,
        line: &Line,
        me: &str,
        casemap: CaseMapping,
        received: bool,
        now: Instant,
    ) {
        if !matches!(line.kind, LineType::Privmsg | LineType::Action) {
            return;
        }
        let Some(joined) = self.joined_mut(id) else {
            return;
        };
        if received && let Some(sender) = &line.nickname {
            let toward_us = line.body.contains(me.trim_matches('_'));
            joined
                .members
                .converse(&casemap.nick(sender), toward_us, now);
        }
        let body = crate::style::body::strip(&line.body);
        let named: Vec<String> = joined
            .members
            .iter()
            .filter(|m| crate::line::mentions(&body, &m.nick, casemap))
            .map(|m| m.nick.clone())
            .collect();
        for nick in named {
            joined
                .members
                .converse(&casemap.nick(&nick), line.from_self, now);
        }
    }

    pub(crate) fn print(&mut self, id: BufferId, line: Line, changes: &mut Vec<Change>) {
        let Some(buffer) = self.get_mut(id) else {
            return;
        };
        if let Some(msgid) = &line.msgid
            && buffer.scrollback.has_msgid(msgid)
        {
            return;
        }
        let line = Rc::new(line);
        buffer.scrollback.push(line.clone());
        changes.push(Change::Line { buffer: id, line });
    }
}
