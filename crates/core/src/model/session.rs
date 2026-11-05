//! What we know on a registered connection, and what each server line does
//! to it and to the network's buffers. A `Session` exists only inside a
//! live `Link` variant, so a disconnect drops all of it; the buffers
//! outlive it and are parted when the link ends.

use std::time::Duration;

use crate::irc::caps::Caps;
use crate::irc::casemap::CaseMapping;
use crate::irc::isupport::Isupport;
use crate::irc::{Message, Source, WireLine};
use crate::line::{Destination, Line, LineType};
use crate::model::Change;
use crate::model::Cx;
use crate::model::buffer::{BufferId, Buffers, Joined, Rejoin, Sync};
use crate::model::members::{Member, MemberDiff, ModeBits};
use crate::model::network::{ConnId, Effect, Out};
use crate::model::registration::Registered;

/// Above this many members the hidden WHO after NAMES is skipped (Textual's
/// `trackUserAwayStatusMaximumChannelSize` default).
const WHO_MAX_MEMBERS: usize = 500;
/// Marks our own WHOX query, so its 354 replies can be told apart.
const WHOX_TOKEN: &str = "152";
/// Textual's `joinKickedChannel` delay (IRCClient.m:7353).
const KICK_REJOIN_DELAY: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub struct Session {
    nick: String,
    /// `user@host` as the server shows us to others, once a line from us
    /// has carried it.
    hostmask: Option<String>,
    account: Option<String>,
    caps: Caps,
    isupport: Isupport,
}

impl Session {
    pub fn new(registered: Registered) -> Self {
        Session {
            nick: registered.nick,
            hostmask: None,
            account: registered.account,
            caps: registered.caps,
            isupport: Isupport::default(),
        }
    }

    pub fn nick(&self) -> &str {
        &self.nick
    }

    pub fn hostmask(&self) -> Option<&str> {
        self.hostmask.as_deref()
    }

    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    pub fn caps(&self) -> &Caps {
        &self.caps
    }

    pub fn isupport(&self) -> &Isupport {
        &self.isupport
    }

    pub fn casemap(&self) -> CaseMapping {
        self.isupport.casemapping
    }

    /// Takes one server line after 001: updates membership and prints.
    pub(crate) fn handle(
        &mut self,
        msg: &Message,
        conn: ConnId,
        buffers: &mut Buffers,
        cx: &mut Cx,
        out: &mut Out,
    ) {
        Handler {
            s: self,
            buffers,
            cx,
            out,
            conn,
        }
        .dispatch(msg);
    }

    fn is_me(&self, nick: &str) -> bool {
        self.casemap().same_nick(nick, &self.nick)
    }

    fn learn_hostmask(&mut self, source: Source<'_>) {
        if let (Some(user), Some(host)) = (source.user, source.host) {
            self.hostmask = Some(format!("{user}@{host}"));
        }
    }
}

struct Handler<'a, 'c> {
    s: &'a mut Session,
    buffers: &'a mut Buffers,
    cx: &'a mut Cx<'c>,
    out: &'a mut Out,
    conn: ConnId,
}

impl Handler<'_, '_> {
    fn dispatch(&mut self, msg: &Message) {
        if let Some(source) = msg.source()
            && self.s.is_me(source.nick)
        {
            self.s.learn_hostmask(source);
        }
        let param = |i| msg.param(i).unwrap_or("");
        match msg.command.as_str() {
            "005" => self.isupport(msg),
            "CAP" => {
                let lines = self.s.caps.on_cap(msg).send;
                self.send_all(lines);
            }
            "PING" | "PONG" | "329" => {}
            "JOIN" => self.join(msg),
            "PART" => self.leave(msg, param(0), None),
            "KICK" => self.leave(msg, param(0), Some(param(1))),
            "QUIT" => self.quit(msg),
            "NICK" => self.nick(msg),
            "MODE" => self.mode(msg),
            "TOPIC" => self.set_topic(msg, param(0), param(1)),
            "332" => self.set_topic(msg, param(1), param(2)),
            "331" => {
                self.set_topic_quietly(param(1), "");
                self.print(msg);
            }
            "324" => self.channel_modes(msg),
            "353" => self.names(msg),
            "366" => self.end_of_names(msg),
            "352" => self.who_reply(msg, param(1), param(6), param(2), param(3), None),
            "354" if param(1) == WHOX_TOKEN => {
                self.who_reply(msg, param(2), param(6), param(3), param(4), Some(param(7)))
            }
            "315" => self.end_of_who(msg),
            "AWAY" => {
                let away = msg.param(0).is_some_and(|r| !r.is_empty());
                self.update_everywhere(msg, |m| m.away = away);
            }
            "CHGHOST" => {
                let (user, host) = (param(0).to_owned(), param(1).to_owned());
                if let Some(source) = msg.source()
                    && self.s.is_me(source.nick)
                {
                    self.s.hostmask = Some(format!("{user}@{host}"));
                }
                self.update_everywhere(msg, |m| {
                    m.user = Some(user.clone());
                    m.host = Some(host.clone());
                });
            }
            "ACCOUNT" => {
                let account = account(param(0));
                if let Some(source) = msg.source()
                    && self.s.is_me(source.nick)
                {
                    self.s.account.clone_from(&account);
                }
                self.update_everywhere(msg, |m| m.account.clone_from(&account));
            }
            _ => self.print(msg),
        }
    }

    fn send_all(&mut self, lines: Vec<WireLine>) {
        let conn = self.conn;
        self.out
            .effects
            .extend(lines.into_iter().map(|line| Effect::Send { conn, line }));
    }

    fn send(&mut self, command: &str, params: &[&str]) {
        // Params come from server lines we already parsed, so they cannot
        // hold a line break; a failure would only drop an optional query.
        if let Ok(line) = Message::new(command, params).to_wire() {
            self.send_all(vec![line]);
        }
    }

    fn changes(&mut self) -> &mut Vec<Change> {
        &mut self.out.changes
    }

    fn prefix(&self) -> Vec<(char, char)> {
        self.s.isupport.prefix().to_vec()
    }

    /// The print funnel: what the message prints, and into which buffers.
    fn print(&mut self, msg: &Message) {
        let stamp = self.cx.stamp();
        if let Some((destination, line)) =
            Line::from_message(msg, &self.s.nick, &self.s.isupport, stamp)
        {
            self.route(destination, line);
        }
    }

    fn route(&mut self, destination: Destination, line: Line) {
        let casemap = self.s.casemap();
        let targets: Vec<BufferId> = match destination {
            // A public message for a channel we are not in is dropped.
            Destination::Channel(name) => self
                .buffers
                .find_joined(casemap, &name)
                .into_iter()
                .collect(),
            // A message from or to an empty nick has no query to go in.
            Destination::Query(nick) if nick.is_empty() => Vec::new(),
            Destination::Query(nick) => vec![self.query(&nick, line.kind != LineType::Notice)],
            Destination::Nick(nick) => self.buffers_of(&nick),
            Destination::Server => vec![self.buffers.console().id()],
        };
        let Some((last, rest)) = targets.split_last() else {
            return;
        };
        for id in rest {
            self.buffers.print(*id, line.clone(), &mut self.out.changes);
        }
        self.buffers.print(*last, line, &mut self.out.changes);
    }

    /// The query with `nick`. A private message opens one; a notice goes to
    /// the console when there is none.
    fn query(&mut self, nick: &str, open: bool) -> BufferId {
        let casemap = self.s.casemap();
        match self.buffers.find_query(casemap, nick) {
            Some(id) => id,
            None if open => {
                let id = self.cx.ids.buffer();
                self.buffers.add_query(id, nick, &mut self.out.changes)
            }
            None => self.buffers.console().id(),
        }
    }

    /// Every buffer `nick` is in: the joined channels it is a member of,
    /// then its query.
    fn buffers_of(&mut self, nick: &str) -> Vec<BufferId> {
        let casemap = self.s.casemap();
        let key = casemap.nick(nick);
        let mut ids: Vec<BufferId> = self
            .buffers
            .all_joined_mut()
            .filter(|(_, joined)| joined.members.contains(&key))
            .map(|(id, _)| id)
            .collect();
        ids.extend(self.buffers.find_query(casemap, nick));
        ids
    }

    fn isupport(&mut self, msg: &Message) {
        if msg.params.len() <= 2 {
            return;
        }
        let tokens: Vec<&str> = msg.params[1..msg.params.len() - 1]
            .iter()
            .map(String::as_str)
            .collect();
        let before = self.s.casemap();
        self.s.isupport.apply(&tokens);
        let casemap = self.s.casemap();
        if casemap != before {
            let mut diffs = Vec::new();
            for (id, joined) in self.buffers.all_joined_mut() {
                diffs.push((id, joined.members.rekey(casemap)));
            }
            for (buffer, diff) in diffs {
                self.changes().push(Change::Members { buffer, diff });
            }
        }
        self.print(msg);
    }

    fn join(&mut self, msg: &Message) {
        let (Some(source), Some(channel)) = (msg.source(), msg.param(0)) else {
            return;
        };
        let casemap = self.s.casemap();
        let id = if self.s.is_me(source.nick) {
            let id = match self.buffers.find_channel(casemap, channel) {
                Some(id) => id,
                None => {
                    let id = self.cx.ids.buffer();
                    self.buffers
                        .add_channel(id, channel, Rejoin::No, &mut self.out.changes)
                }
            };
            // A bouncer replays JOIN for channels we are already in.
            if !self.buffers.join(id, &mut self.out.changes) {
                return;
            }
            self.send("MODE", &[channel]);
            id
        } else {
            match self.buffers.find_joined(casemap, channel) {
                Some(id) => id,
                None => return,
            }
        };
        let mut member = Member::new(source.nick);
        member.user = source.user.map(str::to_owned);
        member.host = source.host.map(str::to_owned);
        // extended-join: `JOIN #chan account :realname`.
        member.account = msg.param(1).and_then(account);
        self.with_members(id, |joined, prefix| {
            joined
                .members
                .insert(casemap.nick(&member.nick), member, prefix)
        });
        self.print(msg);
    }

    /// PART, or KICK of `kicked`.
    fn leave(&mut self, msg: &Message, channel: &str, kicked: Option<&str>) {
        let Some(nick) = kicked.or_else(|| msg.source().map(|s| s.nick)) else {
            return;
        };
        let casemap = self.s.casemap();
        let Some(id) = self.buffers.find_joined(casemap, channel) else {
            return;
        };
        self.print(msg);
        if self.s.is_me(nick) {
            if kicked.is_some() && self.cx.prefs.rejoin_on_kick {
                let at = self.cx.now.mono + KICK_REJOIN_DELAY;
                self.buffers.part(id, Rejoin::At(at), &mut self.out.changes);
                let text = "Attempting to rejoin channel in three seconds";
                let line = Line::client(self.cx.stamp(), text.to_owned());
                self.buffers.print(id, line, &mut self.out.changes);
            } else {
                self.buffers.part(id, Rejoin::No, &mut self.out.changes);
            }
        } else {
            let key = casemap.nick(nick);
            self.with_members(id, |joined, _| joined.members.remove(&key));
        }
    }

    fn quit(&mut self, msg: &Message) {
        let Some(source) = msg.source() else { return };
        self.print(msg);
        let key = self.s.casemap().nick(source.nick);
        self.each_joined(|joined, _| joined.members.remove(&key));
    }

    fn nick(&mut self, msg: &Message) {
        let (Some(source), Some(new)) = (msg.source(), msg.param(0)) else {
            return;
        };
        // Printed first: where it goes depends on the old nick.
        self.print(msg);
        let casemap = self.s.casemap();
        let (old, new_key) = (casemap.nick(source.nick), casemap.nick(new));
        self.each_joined(|joined, prefix| {
            joined.members.rename(&old, new_key.clone(), new, prefix)
        });
        if let Some(query) = self.buffers.find_query(casemap, source.nick)
            && (old == new_key || self.buffers.find_query(casemap, new).is_none())
        {
            self.buffers.rename(query, new, &mut self.out.changes);
        }
        if self.s.is_me(source.nick) {
            new.clone_into(&mut self.s.nick);
        }
    }

    fn mode(&mut self, msg: &Message) {
        let casemap = self.s.casemap();
        if let Some(id) = msg
            .param(0)
            .and_then(|target| self.buffers.find_joined(casemap, target))
        {
            let args: Vec<&str> = msg.params[1..].iter().map(String::as_str).collect();
            self.apply_modes(id, &args);
        }
        self.print(msg);
    }

    fn channel_modes(&mut self, msg: &Message) {
        let casemap = self.s.casemap();
        if let Some(id) = msg
            .param(1)
            .and_then(|channel| self.buffers.find_joined(casemap, channel))
        {
            if let Some(joined) = self.buffers.joined_mut(id) {
                joined.modes.clear();
            }
            let args: Vec<&str> = msg.params[2..].iter().map(String::as_str).collect();
            self.apply_modes(id, &args);
            self.changes().push(Change::BufferChanged(id));
        }
        self.print(msg);
    }

    /// `+ov-k alice bob key`: prefix modes change members, list modes are
    /// not kept, the rest are the channel's modes.
    fn apply_modes(&mut self, id: BufferId, args: &[&str]) {
        let Some((modes, mut params)) = args.split_first().map(|(m, p)| (*m, p.iter())) else {
            return;
        };
        let isupport = self.s.isupport.clone();
        let casemap = isupport.casemapping;
        let mut adding = true;
        let mut changed = false;
        for mode in modes.chars() {
            match mode {
                '+' => adding = true,
                '-' => adding = false,
                _ => {
                    let param = isupport
                        .takes_param(mode, adding)
                        .then(|| params.next())
                        .flatten()
                        .copied();
                    if let Some(rank) = isupport.rank_of_mode(mode) {
                        let Some(nick) = param else { continue };
                        let key = casemap.nick(nick);
                        self.with_members(id, |joined, prefix| {
                            joined.members.update(&key, prefix, |m| {
                                m.modes = if adding {
                                    m.modes.with(rank)
                                } else {
                                    m.modes.without(rank)
                                };
                            })
                        });
                    } else if !isupport.is_list_mode(mode)
                        && let Some(joined) = self.buffers.joined_mut(id)
                    {
                        if adding {
                            joined.modes.insert(mode, param.map(str::to_owned));
                        } else {
                            joined.modes.remove(&mode);
                        }
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.changes().push(Change::BufferChanged(id));
        }
    }

    fn set_topic(&mut self, msg: &Message, channel: &str, topic: &str) {
        self.set_topic_quietly(channel, topic);
        self.print(msg);
    }

    fn set_topic_quietly(&mut self, channel: &str, topic: &str) {
        let casemap = self.s.casemap();
        let Some(id) = self.buffers.find_joined(casemap, channel) else {
            return;
        };
        if let Some(joined) = self.buffers.joined_mut(id) {
            joined.topic = (!topic.is_empty()).then(|| topic.to_owned());
            self.changes().push(Change::BufferChanged(id));
        }
    }

    /// 353: `= #chan :@+alice!a@host bob`, with every prefix under
    /// multi-prefix and the hostmask under userhost-in-names.
    fn names(&mut self, msg: &Message) {
        let casemap = self.s.casemap();
        let (Some(channel), Some(list)) = (msg.param(2), msg.param(3)) else {
            return;
        };
        let Some(id) = self.buffers.find_joined(casemap, channel) else {
            return;
        };
        let isupport = &self.s.isupport;
        let entries: Vec<Member> = list
            .split(' ')
            .filter_map(|token| {
                let mut modes = ModeBits::default();
                let mut rest = token;
                while let Some(c) = rest.chars().next()
                    && let Some(rank) = isupport
                        .mode_of_symbol(c)
                        .and_then(|m| isupport.rank_of_mode(m))
                {
                    modes = modes.with(rank);
                    rest = &rest[c.len_utf8()..];
                }
                let source = Source::parse(rest);
                if source.nick.is_empty() {
                    return None;
                }
                let mut member = Member::new(source.nick);
                member.modes = modes;
                member.user = source.user.map(str::to_owned);
                member.host = source.host.map(str::to_owned);
                Some(member)
            })
            .collect();
        if let Some(joined) = self.buffers.joined_mut(id) {
            joined.names.get_or_insert_default().extend(entries);
        }
    }

    fn end_of_names(&mut self, msg: &Message) {
        let casemap = self.s.casemap();
        let Some(id) = msg
            .param(1)
            .and_then(|channel| self.buffers.find_joined(casemap, channel))
        else {
            return;
        };
        let away_notify = self.s.caps.is_enabled("away-notify");
        let Some(joined) = self.buffers.joined_mut(id) else {
            return;
        };
        if let Some(names) = joined.names.take() {
            let entries = names
                .into_iter()
                .map(|m| (casemap.nick(&m.nick), m))
                .collect();
            let diff = joined.members.replace_all(entries);
            let small = joined.members.len() < WHO_MAX_MEMBERS;
            let first = joined.sync == Sync::Names;
            joined.sync = if first && small && !away_notify {
                Sync::Who
            } else {
                Sync::Done
            };
            let who = joined.sync == Sync::Who;
            self.changes().push(Change::Members { buffer: id, diff });
            if who {
                let channel = msg.param(1).unwrap_or_default().to_owned();
                if self.s.isupport.whox {
                    let fields = format!("%tcuhnfa,{WHOX_TOKEN}");
                    self.send("WHO", &[&channel, &fields]);
                } else {
                    self.send("WHO", &[&channel]);
                }
            }
        }
    }

    /// The joined channel `channel` if our hidden WHO for it is out.
    fn hidden_who(&mut self, channel: &str) -> Option<BufferId> {
        let casemap = self.s.casemap();
        self.buffers.find_joined(casemap, channel).filter(|id| {
            self.buffers
                .joined_mut(*id)
                .is_some_and(|j| j.sync == Sync::Who)
        })
    }

    /// 352 and our 354: away, user and host for a member. Replies to a WHO
    /// the user asked for are printed instead.
    fn who_reply(
        &mut self,
        msg: &Message,
        channel: &str,
        flags: &str,
        user: &str,
        host: &str,
        account_name: Option<&str>,
    ) {
        let Some(id) = self.hidden_who(channel) else {
            self.print(msg);
            return;
        };
        // The nick is the sixth parameter in 352 and in our WHOX field order.
        let Some(nick) = msg.param(5) else { return };
        let key = self.s.casemap().nick(nick);
        let away = flags.starts_with('G');
        self.with_members(id, |joined, prefix| {
            joined.members.update(&key, prefix, |m| {
                m.away = away;
                m.user = Some(user.to_owned());
                m.host = Some(host.to_owned());
                if let Some(name) = account_name {
                    // WHOX writes "0" for no account.
                    m.account = (name != "0").then(|| name.to_owned());
                }
            })
        });
    }

    fn end_of_who(&mut self, msg: &Message) {
        match self.hidden_who(msg.param(1).unwrap_or_default()) {
            Some(id) => {
                if let Some(joined) = self.buffers.joined_mut(id) {
                    joined.sync = Sync::Done;
                }
            }
            None => self.print(msg),
        }
    }

    /// AWAY, CHGHOST and ACCOUNT: the sender's entry in every channel.
    fn update_everywhere(&mut self, msg: &Message, edit: impl Fn(&mut Member)) {
        let Some(source) = msg.source() else { return };
        let key = self.s.casemap().nick(source.nick);
        self.each_joined(|joined, prefix| joined.members.update(&key, prefix, &edit));
    }

    fn with_members(
        &mut self,
        id: BufferId,
        change: impl FnOnce(&mut Joined, &[(char, char)]) -> Option<MemberDiff>,
    ) {
        let prefix = self.prefix();
        if let Some(diff) = self.buffers.joined_mut(id).and_then(|j| change(j, &prefix)) {
            self.changes().push(Change::Members { buffer: id, diff });
        }
    }

    fn each_joined(
        &mut self,
        mut change: impl FnMut(&mut Joined, &[(char, char)]) -> Option<MemberDiff>,
    ) {
        let prefix = self.prefix();
        let mut diffs = Vec::new();
        for (id, joined) in self.buffers.all_joined_mut() {
            if let Some(diff) = change(joined, &prefix) {
                diffs.push(Change::Members { buffer: id, diff });
            }
        }
        self.changes().extend(diffs);
    }
}

/// An account name, or `None` for `*` (logged out) and an empty field.
fn account(name: &str) -> Option<String> {
    (!name.is_empty() && name != "*").then(|| name.to_owned())
}
