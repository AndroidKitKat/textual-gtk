//! What Return and Tab do with the input: Textual's `inputText:asCommand:`
//! and `sendCommand:` (`IRCClient.m:2721-4994`), and Tab completion fed by
//! the focused buffer. Command behaviour follows Textual case by case,
//! including its silent no-ops when not connected (`NSAssertReturnLoopBreak`).

use std::ops::Range;

use crate::input::commands::{
    self, COMMANDS, KickBan, Spec, Typed, Verb, classify, split_lines, token,
};
use crate::input::completion::{self, Source};
use crate::irc::isupport::Isupport;
use crate::irc::{Message, WireLine};
use crate::model::buffer::{Body, BufferId};
use crate::model::network::{self, Network, Speech};
use crate::model::workspace::{Out, UiEffect, Workspace};
use crate::model::{Cx, LEAVING_COMMENT, Now};

/// Textual's `IRC[g01-qn]`.
const CHANNEL_ONLY: &str = "This command can only be used with a channel.";
/// Textual's `IRC[zef-q9]`.
const BAD_ARGUMENTS: &str = "One or more arguments are not properly formatted for this command.";
/// Textual's `ChannelOperatorDefaultLocalization -> Kick Reason` default.
const KICK_REASON: &str = "Your behavior is not conducive to the desired environment.";
/// Textual's `IRC[xog-in]`.
const AWAY_MESSAGE: &str = "Be back later";

/// What a command leaves for the workspace once the network's step is done.
enum After {
    Nothing,
    Focus(BufferId),
    Close(BufferId),
}

impl Workspace {
    pub(super) fn submit(
        &mut self,
        buffer: BufferId,
        text: &str,
        speech: Speech,
        confirmed: bool,
        now: Now,
        out: &mut Out,
    ) {
        self.input.completion.clear();
        self.input.field.clear();
        if text.is_empty() {
            return;
        }
        if !confirmed {
            if self.prefs.input_history_per_buffer {
                self.input.buffers.entry(buffer).or_default().0.add(text);
            } else {
                self.input.global.add(text);
            }
            if commands::is_flood(text) {
                out.ui.push(UiEffect::ConfirmFlood {
                    buffer,
                    text: text.to_owned(),
                    as_action: speech == Speech::Action,
                });
                return;
            }
        }
        for line in split_lines(text) {
            // A line can close the buffer the next one was typed in.
            let Some((net, buf)) = self.buffer(buffer) else {
                return;
            };
            let server = net.id();
            match classify(line, buf.is_console()) {
                Typed::Command(command) => self.command(server, buffer, command, now, out),
                Typed::Text("") => {}
                Typed::Text(text) => {
                    self.on_network(server, now, out, |net, cx, o| {
                        let Some(target) = net.buffers().get(buffer).map(|b| b.name().to_owned())
                        else {
                            return;
                        };
                        net.send_text(&target, speech, text, Some(buffer), cx, o);
                    });
                }
            }
        }
    }

    fn command(
        &mut self,
        server: network::ServerId,
        buffer: BufferId,
        line: &str,
        now: Now,
        out: &mut Out,
    ) {
        let line = line.strip_prefix('/').unwrap_or(line);
        let (typed, args) = token(line);
        if typed.is_empty() {
            return;
        }
        let after = self.on_network(server, now, out, |net, cx, o| {
            let mut run = Run {
                net,
                cx,
                out: o,
                here: buffer,
                typed,
            };
            match commands::find(typed) {
                Some(spec) => run.verb(spec, args),
                None => {
                    // Textual's fallthrough: the command uppercased, the
                    // rest as typed (`IRCClient.m:4989-5001`).
                    run.raw(&format!("{} {args}", typed.to_uppercase()));
                    After::Nothing
                }
            }
        });
        match after {
            Some(After::Focus(id)) => self.select(id, out),
            Some(After::Close(id)) => self.close(id, now, out),
            Some(After::Nothing) | None => {}
        }
    }

    pub(super) fn complete(
        &mut self,
        text: String,
        selection: Range<usize>,
        backward: bool,
        now: Now,
        out: &mut Out,
    ) {
        let Some(focus) = self.focus else { return };
        let Some(net) = self
            .networks
            .iter_mut()
            .find(|n| n.buffers().get(focus).is_some())
        else {
            return;
        };
        let casemap = net.casemap();
        let me = net.nick().to_owned();
        let mut channels: Vec<String> = Vec::new();
        let mut kind = None;
        for b in net.buffers().iter() {
            match b.body() {
                Body::Console => {}
                _ if b.id() == focus => channels.insert(0, b.name().to_owned()),
                _ => channels.push(b.name().to_owned()),
            }
            if b.id() == focus {
                kind = Some(match b.body() {
                    Body::Console => Kind::Console,
                    Body::Channel(_) => Kind::Channel,
                    Body::Query => Kind::Query(b.name().to_owned()),
                });
            }
        }
        let source = Source {
            commands: COMMANDS.iter().map(|s| s.name.to_owned()).collect(),
            channels,
            network: net.session().and_then(|s| s.isupport().network.clone()),
        };
        let members = || -> Option<Vec<completion::Member>> {
            let weighed = match kind? {
                Kind::Console => return None,
                Kind::Query(nick) => {
                    // Textual's query holds two members: the other user and us.
                    let mut both = vec![(nick, 0.0), (me, 0.0)];
                    both.sort_by_key(|(n, _)| casemap.nick(n).as_str().to_owned());
                    both
                }
                Kind::Channel => net
                    .buffers_mut()
                    .joined_mut(focus)
                    .map(|j| j.members.weighed(now.mono))
                    .unwrap_or_default(),
            };
            Some(
                weighed
                    .into_iter()
                    .map(|(nick, weight)| completion::Member { nick, weight })
                    .collect(),
            )
        };
        let edit = self.input.completion.complete(
            &text,
            selection,
            backward,
            &source,
            members,
            &self.prefs.completion,
        );
        if let Some(edit) = edit {
            self.set_input(text, edit.text, edit.caret, out);
        }
    }
}

enum Kind {
    Console,
    Channel,
    Query(String),
}

/// One command, run against the network of the buffer it was typed in.
struct Run<'a, 'b, 'c> {
    net: &'a mut Network,
    cx: &'a mut Cx<'c>,
    out: &'a mut network::Out,
    /// Where it was typed: Textual's `targetChannel` when not the console.
    here: BufferId,
    /// The command as typed, for its "Invalid syntax" line.
    typed: &'b str,
}

impl Run<'_, '_, '_> {
    fn verb(&mut self, spec: &Spec, args: &str) -> After {
        let logged_in = self.net.is_logged_in();
        let connected = self.net.is_connected();
        match spec.verb {
            Verb::Join if logged_in => return self.join(spec, args),
            Verb::Part if logged_in => return self.part(args),
            Verb::Msg => return self.message(spec, args, Speech::Privmsg),
            Verb::Notice => return self.message(spec, args, Speech::Notice),
            Verb::Me => return self.me(spec, args),
            Verb::Query => return self.query(spec, args),
            // Connected, not registered: Textual lets the nick change
            // before 001 (`IRCClient.m:4008-4011`).
            Verb::Nick if connected => match token(args).0 {
                "" => self.invalid(spec),
                nick => self.send("NICK", &[nick]),
            },
            Verb::Quit if connected => self.disconnect(args),
            Verb::Topic if logged_in => self.topic(args),
            Verb::Mode if logged_in => self.mode(spec, args),
            Verb::Kick(what) if logged_in => self.kick_ban(spec, args, what),
            Verb::Member { mode, set } if logged_in => self.member_mode(spec, args, mode, set),
            Verb::Ctcp { reply } if logged_in => self.ctcp(spec, args, reply),
            Verb::Names if logged_in => match args {
                "" => self.invalid(spec),
                // MODE-like: Textual adds no colon (`outgoingColonIndex` 999).
                names => self.raw(&format!("NAMES {names}")),
            },
            Verb::Invite if logged_in => self.invite(spec, args),
            Verb::Whois if logged_in => self.whois(spec, args),
            Verb::Away if logged_in => {
                if self.net.session().is_some_and(|s| s.is_away()) {
                    self.send("AWAY", &[]);
                } else {
                    let message = if args.is_empty() { AWAY_MESSAGE } else { args };
                    self.send("AWAY", &[message]);
                }
            }
            Verb::Close => return self.close(args),
            Verb::Conn => self.conn(args),
            Verb::Raw if connected => match args {
                "" => self.invalid(spec),
                line => self.raw(line),
            },
            _ => {}
        }
        After::Nothing
    }

    fn print(&mut self, text: &str) {
        let here = self.here;
        self.net.print_client(here, text, self.cx, self.out);
    }

    fn invalid(&mut self, spec: &Spec) {
        let text = commands::invalid_syntax(self.typed, spec);
        self.print(&text);
    }

    fn send(&mut self, command: &str, params: &[&str]) {
        match Message::new(command, params).to_wire() {
            Ok(line) => self.net.send_line(line, self.cx, self.out),
            Err(e) => self.print(&format!("Not sent: {e}")),
        }
    }

    /// A line typed whole: `/raw`, `/quote` and unknown commands.
    fn raw(&mut self, line: &str) {
        match WireLine::typed(line) {
            Ok(line) => self.net.send_line(line, self.cx, self.out),
            Err(e) => self.print(&format!("Not sent: {e}")),
        }
    }

    fn isupport(&self) -> Isupport {
        self.net
            .session()
            .map(|s| s.isupport().clone())
            .unwrap_or_default()
    }

    fn is_channel(&self, name: &str) -> bool {
        self.isupport().is_channel(name)
    }

    /// Textual's `stringIsNickname:`.
    fn is_nickname(&self, name: &str) -> bool {
        let isupport = self.isupport();
        !name.is_empty()
            && name != "*"
            && !name.contains(['!', '@', '\0', ' ', '\r', '\n'])
            && !isupport.is_channel(name)
            && isupport
                .nicklen
                .is_none_or(|n| n == 0 || name.chars().count() <= n)
    }

    /// The channel this was typed in, if it was typed in one.
    fn channel_here(&self) -> Option<String> {
        let b = self.net.buffers().get(self.here)?;
        matches!(b.body(), Body::Channel(_)).then(|| b.name().to_owned())
    }

    fn query_here(&self) -> Option<String> {
        let b = self.net.buffers().get(self.here)?;
        matches!(b.body(), Body::Query).then(|| b.name().to_owned())
    }

    /// `/join [#chan[,#chan]] [keys]` (`IRCClient.m:3683-3713`): a name
    /// that is not a channel gets `#`, the first channel's buffer is made
    /// at once and focused, and an already joined lone channel is only
    /// focused.
    fn join(&mut self, spec: &Spec, args: &str) -> After {
        let (target, keys) = if args.is_empty() {
            match self.channel_here() {
                Some(name) => (name, ""),
                None => {
                    self.print(CHANNEL_ONLY);
                    return After::Nothing;
                }
            }
        } else {
            let (target, keys) = token(args);
            if target.is_empty() {
                self.invalid(spec);
                return After::Nothing;
            }
            let target = if self.is_channel(target) || target == "0" {
                target.to_owned()
            } else {
                format!("#{target}")
            };
            (target, keys)
        };
        let names: Vec<&str> = target.split(',').collect();
        let first = names
            .iter()
            .find(|n| self.is_channel(n))
            .map(|n| (*n).to_owned());
        let shown = first.map(|name| self.net.open_channel(&name, self.cx.ids, self.out));
        let lone_joined = names.len() == 1
            && shown.is_some_and(|id| {
                self.net
                    .buffers()
                    .get(id)
                    .is_some_and(|b| b.joined().is_some())
            });
        if !lone_joined {
            let mut params = vec![target.as_str()];
            if !keys.is_empty() {
                params.push(keys);
            }
            self.send("JOIN", &params);
        }
        shown.map_or(After::Nothing, After::Focus)
    }

    /// `/part [#chan] [comment]`: in a query it closes the query, in the
    /// console it does nothing (`IRCClient.m:3887-3915`).
    fn part(&mut self, args: &str) -> After {
        let (first, rest) = token(args);
        let (channel, reason) = if self.is_channel(first) {
            (first.to_owned(), rest)
        } else if let Some(name) = self.channel_here() {
            (name, args)
        } else if self.query_here().is_some() {
            return After::Close(self.here);
        } else {
            return After::Nothing;
        };
        let reason = if reason.is_empty() {
            LEAVING_COMMENT
        } else {
            reason
        };
        self.send("PART", &[&channel, reason]);
        After::Nothing
    }

    /// `/msg`, `/notice`, and `/me` in the console (`IRCClient.m:4720-4943`):
    /// every target gets the message; a nick without a query gets one, and
    /// the first target is focused (`FocusSelectionOnMessageCommandExecution`).
    /// A channel we have no buffer for is sent to without an echo.
    fn message(&mut self, spec: &Spec, args: &str, speech: Speech) -> After {
        if !self.net.is_logged_in() {
            let console = self.net.buffers().console().id();
            self.net
                .print_client(console, network::NOT_CONNECTED, self.cx, self.out);
            return After::Nothing;
        }
        let (targets, text) = token(args);
        if targets.is_empty() {
            self.invalid(spec);
            return After::Nothing;
        }
        let text = match text {
            "" if speech == Speech::Action => " ",
            "" => return After::Nothing,
            text => text,
        };
        let mut focus = None;
        for target in targets.split(',') {
            let isupport = self.isupport();
            let (_, name) = isupport.split_statusmsg(target);
            let casemap = self.net.casemap();
            let mut echo = self
                .net
                .buffers()
                .find_channel(casemap, name)
                .or_else(|| self.net.buffers().find_query(casemap, name));
            if echo.is_none() && self.is_nickname(name) {
                echo = Some(self.net.open_query(name, self.cx.ids, self.out));
            }
            if focus.is_none() {
                focus = echo;
            }
            self.net
                .send_text(target, speech, text, echo, self.cx, self.out);
        }
        match focus {
            Some(id) if self.cx.prefs.focus_on_message_command => After::Focus(id),
            _ => After::Nothing,
        }
    }

    /// `/me`: to the channel or query it was typed in; in the console it
    /// is `/msg` as an action, an empty one being a space.
    fn me(&mut self, spec: &Spec, args: &str) -> After {
        if !self.net.is_logged_in() {
            let console = self.net.buffers().console().id();
            self.net
                .print_client(console, network::NOT_CONNECTED, self.cx, self.out);
            return After::Nothing;
        }
        let Some(target) = self.channel_here().or_else(|| self.query_here()) else {
            return self.message(spec, args, Speech::Action);
        };
        let text = if args.is_empty() { " " } else { args };
        let here = self.here;
        self.net
            .send_text(&target, Speech::Action, text, Some(here), self.cx, self.out);
        After::Nothing
    }

    /// `/query nick [text]` opens and focuses the query, connected or not.
    fn query(&mut self, spec: &Spec, args: &str) -> After {
        let (nick, text) = token(args);
        if nick.is_empty() {
            self.invalid(spec);
            return After::Nothing;
        }
        if !self.is_nickname(nick) {
            self.print(BAD_ARGUMENTS);
            return After::Nothing;
        }
        let id = self.net.open_query(nick, self.cx.ids, self.out);
        if !text.is_empty() {
            self.net
                .send_text(nick, Speech::Privmsg, text, Some(id), self.cx, self.out);
        }
        After::Focus(id)
    }

    /// `/conn [server]` (`IRCClient.m:3197-3225`): a link that is up or on
    /// its way quits and dials again, an absent one dials. A server given
    /// is the next dial's address.
    fn conn(&mut self, args: &str) {
        let host = token(args).0.to_lowercase();
        if !host.is_empty() {
            if !is_internet_address(&host) {
                self.print(BAD_ARGUMENTS);
                return;
            }
            self.net.override_address(host);
        }
        if self.net.is_connected() || matches!(self.net.link(), network::Link::Connecting { .. }) {
            self.disconnect("");
        }
        self.net.update(network::Input::Connect, self.cx, self.out);
    }

    fn disconnect(&mut self, reason: &str) {
        let reason = if reason.is_empty() {
            LEAVING_COMMENT
        } else {
            reason
        };
        let input = network::Input::Disconnect {
            reason: Some(reason.to_owned()),
        };
        self.net.update(input, self.cx, self.out);
    }

    /// `/topic [#chan] [text]`: no text asks the server for the topic.
    fn topic(&mut self, args: &str) {
        let (first, rest) = token(args);
        let (channel, topic) = if self.is_channel(first) {
            (first.to_owned(), rest)
        } else if let Some(name) = self.channel_here() {
            (name, args)
        } else {
            return;
        };
        if topic.is_empty() {
            self.send("TOPIC", &[&channel]);
            return;
        }
        if let Some(max) = self.isupport().topiclen
            && topic.encode_utf16().count() > max
        {
            let text = format!(
                "You have exceeded the maximum topic length for {} which is {max} characters. \
                 The end of your topic may have been cut off.",
                self.network_name()
            );
            self.print(&text);
        }
        self.send("TOPIC", &[&channel, topic]);
    }

    /// `/mode [target] [modes]`: modes alone apply to the channel it was
    /// typed in. Textual repeats an explicit target (`MODE #c #c +o x`,
    /// `IRCClient.m:3935`); it is sent once here.
    fn mode(&mut self, spec: &Spec, args: &str) {
        let (target, modes) = if args.is_empty() || args.starts_with(['+', '-']) {
            match self.channel_here() {
                Some(name) => (name, args),
                None => {
                    self.invalid(spec);
                    return;
                }
            }
        } else {
            let (target, modes) = token(args);
            (target.to_owned(), modes)
        };
        let mut params = vec![target.as_str()];
        params.extend(modes.split_whitespace());
        self.send("MODE", &params);
    }

    /// The channel a command names first, or the one it was typed in, and
    /// what follows. `None` after saying "only with a channel".
    fn channel_and_rest<'s>(&mut self, args: &'s str) -> Option<(String, &'s str)> {
        let (first, rest) = token(args);
        if self.is_channel(first) {
            return Some((first.to_owned(), rest));
        }
        let here = self.channel_here();
        if here.is_none() {
            self.print(CHANNEL_ONLY);
        }
        here.map(|name| (name, args))
    }

    /// `/kick`, `/ban`, `/unban`, `/quiet`, `/unquiet`, `/kickban` and `/kb`
    /// with `[#chan] nick [comment]` (`IRCClient.m:3738-3832`). A ban names
    /// the member's mask when they are in the channel, the nick otherwise.
    fn kick_ban(&mut self, spec: &Spec, args: &str, what: KickBan) {
        let Some((channel, rest)) = self.channel_and_rest(args) else {
            return;
        };
        let (nick, reason) = token(rest);
        if nick.is_empty() {
            self.invalid(spec);
            return;
        }
        let ban = match what {
            KickBan::Kick => None,
            KickBan::Ban | KickBan::KickBan => Some(('+', 'b')),
            KickBan::Unban => Some(('-', 'b')),
            KickBan::Quiet => Some(('+', 'q')),
            KickBan::Unquiet => Some(('-', 'q')),
        };
        if let Some((sign, mode)) = ban {
            if self.isupport().symbol_of_mode(mode).is_some() {
                self.print(&not_supported(mode));
                return;
            }
            let mask = self.ban_mask(&channel, nick);
            self.send("MODE", &[&channel, &format!("{sign}{mode}"), &mask]);
        }
        if matches!(what, KickBan::Kick | KickBan::KickBan) {
            let reason = if reason.is_empty() {
                KICK_REASON
            } else {
                reason
            };
            if let Some(max) = self.isupport().kicklen
                && reason.encode_utf16().count() > max
            {
                let text = format!(
                    "You have exceeded the maximum kick message length for {} which is {max} \
                     characters. The end of your kick message may have been cut off.",
                    self.network_name()
                );
                self.print(&text);
            }
            self.send("KICK", &[&channel, nick, reason]);
        }
    }

    /// Textual's `banMask` in its default format (`*!user@host`), or
    /// `nick!*@*` for a member whose address is unknown, or the nick as
    /// typed for someone not in the channel.
    fn ban_mask(&self, channel: &str, nick: &str) -> String {
        let casemap = self.net.casemap();
        let member = self
            .net
            .buffers()
            .find_channel(casemap, channel)
            .and_then(|id| self.net.buffers().get(id))
            .and_then(|b| b.joined())
            .and_then(|j| j.members().get(&casemap.nick(nick)).cloned());
        match member {
            Some(m) => match (m.user, m.host) {
                (Some(user), Some(host)) => format!("*!{user}@{host}"),
                _ => format!("{}!*@*", m.nick),
            },
            None => nick.to_owned(),
        }
    }

    /// `/op`, `/deop`, `/halfop`, `/dehalfop`, `/voice`, `/devoice` with
    /// `[#chan] nick...` (`IRCClient.m:3282-3340`): as many nicks per MODE
    /// as the server's MODES allows.
    fn member_mode(&mut self, spec: &Spec, args: &str, mode: char, set: bool) {
        if self.isupport().symbol_of_mode(mode).is_none() {
            self.print(&not_supported(mode));
            return;
        }
        let Some((channel, nicks)) = self.channel_and_rest(args) else {
            return;
        };
        let nicks: Vec<&str> = nicks.split_whitespace().collect();
        if nicks.is_empty() {
            self.invalid(spec);
            return;
        }
        let sign = if set { '+' } else { '-' };
        for chunk in nicks.chunks(self.isupport().modes) {
            let modes: String = std::iter::once(sign)
                .chain(std::iter::repeat_n(mode, chunk.len()))
                .collect();
            let mut params = vec![channel.as_str(), &modes];
            params.extend(chunk);
            self.send("MODE", &params);
        }
    }

    /// `/ctcp nick command [extra]` and `/ctcpreply` (`IRCClient.m:3227-3262`).
    /// `PING` carries the time, as Textual's `sendCTCPPing:`.
    fn ctcp(&mut self, spec: &Spec, args: &str, reply: bool) {
        let (target, rest) = token(args);
        let (command, extra) = token(rest);
        if command.is_empty() {
            self.invalid(spec);
            return;
        }
        let command = command.to_uppercase();
        let ping;
        let extra = if !reply && command == "PING" {
            let micros = self.cx.now.wall.timestamp_micros();
            ping = format!(
                "{}.{:06}",
                micros.div_euclid(1_000_000),
                micros.rem_euclid(1_000_000)
            );
            ping.as_str()
        } else {
            extra
        };
        // Textual writes `command + " " + extra` even for an empty extra;
        // the stray space before the closing \x01 is left out here.
        let body = if extra.is_empty() {
            format!("\x01{command}\x01")
        } else {
            format!("\x01{command} {extra}\x01")
        };
        let verb = if reply { "NOTICE" } else { "PRIVMSG" };
        self.send(verb, &[target, &body]);
    }

    /// Textual's `networkNameAlt`: the network's name, or the server's.
    fn network_name(&self) -> String {
        self.net
            .session()
            .and_then(|s| s.isupport().network.clone())
            .unwrap_or_else(|| self.net.config().name.clone())
    }

    /// `/invite nick... [#chan]`: one INVITE per nick.
    fn invite(&mut self, spec: &Spec, args: &str) {
        let mut words: Vec<&str> = args.split_whitespace().collect();
        if words.is_empty() {
            self.invalid(spec);
            return;
        }
        let channel = if words.last().is_some_and(|w| self.is_channel(w)) {
            words.pop().map(str::to_owned)
        } else {
            self.channel_here()
        };
        let Some(channel) = channel else {
            self.print(CHANNEL_ONLY);
            return;
        };
        for nick in words {
            if self.is_nickname(nick) {
                self.send("INVITE", &[nick, &channel]);
            }
        }
    }

    /// `/whois [nick] [nick]`: a lone nick is sent twice, which asks its
    /// own server; in a query with no nick, the query's.
    fn whois(&mut self, spec: &Spec, args: &str) {
        let (first, rest) = token(args);
        let nick = match first {
            "" => match self.query_here() {
                Some(nick) => nick,
                None => {
                    self.invalid(spec);
                    return;
                }
            },
            nick => nick.to_owned(),
        };
        let second = match token(rest).0 {
            "" => nick.clone(),
            other => other.to_owned(),
        };
        self.send("WHOIS", &[&nick, &second]);
    }

    /// `/close [name]`: the buffer it was typed in, or the named one.
    fn close(&mut self, args: &str) -> After {
        let (name, _) = token(args);
        if name.is_empty() {
            let console = self
                .net
                .buffers()
                .get(self.here)
                .is_some_and(|b| b.is_console());
            return if console {
                After::Nothing
            } else {
                After::Close(self.here)
            };
        }
        let casemap = self.net.casemap();
        let found = self
            .net
            .buffers()
            .find_channel(casemap, name)
            .or_else(|| self.net.buffers().find_query(casemap, name));
        match found {
            Some(id) => After::Close(id),
            None => {
                self.print(&format!("Cannot find channel named \u{201C}{name}\u{201C}"));
                After::Nothing
            }
        }
    }
}

/// Textual's `isValidInternetAddress`: an IP address, `localhost`, or
/// only letters, digits, `_`, `-` and `.`.
fn is_internet_address(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
        || host == "localhost"
        || host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Textual's `IRC[dwi-d1]`, which names the mode with `+` either way.
fn not_supported(mode: char) -> String {
    format!("The mode \x02+{mode}\x02 is not supported on this server")
}
