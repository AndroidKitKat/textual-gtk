//! One printed line of a chat view (Textual's `TVCLogLine`), built from an
//! IRC message and rendered through the style's line template.

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Local, TimeZone};
use md5::{Digest, Md5};

use crate::irc::casemap::CaseMapping;
use crate::irc::isupport::Isupport;
use crate::irc::{Message, Source};
use crate::style::template::Value;
use crate::style::{NicknameColors, Style, body};

const DEFAULT_NICKNAME_FORMAT: &str = "<%@%n>";
const DEFAULT_TIMESTAMP_FORMAT: &str = "[%H:%M:%S]";
/// `\x0F`, which Textual appends to user-supplied text inside its own
/// formatted strings so that text's formatting cannot leak out.
const STOP: &str = "\x0F";
/// Textual's long date style: "November 5, 2026 at 5:49:09 PM".
const LONG_DATE: &str = "%B %-d, %Y at %-I:%M:%S %p";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineType {
    Privmsg,
    Action,
    Notice,
    Join,
    Part,
    Quit,
    Kick,
    Nick,
    Mode,
    Topic,
    Debug,
}

impl LineType {
    pub fn as_str(self) -> &'static str {
        match self {
            LineType::Privmsg => "privmsg",
            LineType::Action => "action",
            LineType::Notice => "notice",
            LineType::Join => "join",
            LineType::Part => "part",
            LineType::Quit => "quit",
            LineType::Kick => "kick",
            LineType::Nick => "nick",
            LineType::Mode => "mode",
            LineType::Topic => "topic",
            LineType::Debug => "debug",
        }
    }

    /// Spoken lines render through `newMessagePostedWithSender`, events
    /// through `newMessagePostedWithoutSender` (Textual's
    /// `TemplateLineTypes.plist`).
    pub fn is_spoken(self) -> bool {
        matches!(
            self,
            LineType::Privmsg | LineType::Action | LineType::Notice
        )
    }
}

/// A line's identity within one workspace. Assigned in order, so two runs
/// of one script give the same ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LineId(u64);

impl LineId {
    pub const fn new(n: u64) -> Self {
        LineId(n)
    }
}

impl fmt::Display for LineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What the caller supplies for every new line. The core never reads the
/// clock or a global counter, so two runs of one script print equal lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub id: LineId,
    pub at: DateTime<Local>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub id: LineId,
    pub received_at: DateTime<Local>,
    pub kind: LineType,
    /// The IRC command or numeric the line came from; `CLIENT_COMMAND` for
    /// what the client says itself.
    pub command: String,
    /// Who spoke, for spoken lines.
    pub nickname: Option<String>,
    pub from_self: bool,
    /// Body text, still carrying IRC formatting codes.
    pub body: String,
    pub highlight: bool,
    /// The server's `msgid` tag, which identifies a message delivered twice.
    pub msgid: Option<String>,
}

/// The command of a line the client says itself (Textual's
/// `TVCLogLineDefaultCommandValue`).
pub const CLIENT_COMMAND: &str = "-100";

/// What kind of buffer a line belongs in. The session turns this into
/// buffers using what it knows about membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    Channel(String),
    /// A private conversation with this nick.
    Query(String),
    /// Every buffer this nick is in: shared channels and its query (QUIT,
    /// NICK).
    Nick(String),
    Server,
}

impl Line {
    pub fn new(
        stamp: Stamp,
        kind: LineType,
        command: &str,
        nickname: Option<&str>,
        body: String,
    ) -> Self {
        Line {
            id: stamp.id,
            received_at: stamp.at,
            kind,
            command: command.to_owned(),
            nickname: nickname.map(str::to_owned),
            from_self: false,
            body,
            highlight: false,
            msgid: None,
        }
    }

    /// A line the client says itself, such as a connection notice.
    pub fn client(stamp: Stamp, text: String) -> Self {
        Line::new(stamp, LineType::Debug, CLIENT_COMMAND, None, text)
    }

    /// The line a server message prints, or `None` for messages that print
    /// nothing. `me` is our nick, compared under the server's casemap.
    pub fn from_message(
        message: &Message,
        me: &str,
        isupport: &Isupport,
        stamp: Stamp,
    ) -> Option<(Destination, Line)> {
        let casemap = isupport.casemapping;
        let source = message.source();
        let nick = source.map_or("", |s| s.nick);
        let user = source.and_then(|s| s.user).unwrap_or("");
        let host = source.and_then(|s| s.host).unwrap_or("");
        let param = |i| message.param(i).unwrap_or("");
        let params_from = |i| message.params.get(i..).unwrap_or_default().join(" ");
        let reason = |i| {
            message
                .param(i)
                .filter(|r| !r.is_empty())
                .map(|r| format!(" ({r}{STOP})"))
                .unwrap_or_default()
        };
        let mine = !nick.is_empty() && casemap.same_nick(nick, me);
        let channel = |i| Destination::Channel(param(i).to_owned());
        let event = |kind, body| Line::new(stamp, kind, &message.command, None, body);

        let (destination, mut line) = match message.command.as_str() {
            "PRIVMSG" | "NOTICE" => {
                let (_, target) = isupport.split_statusmsg(param(0));
                let text = param(1);
                let ctcp = text
                    .strip_prefix('\x01')
                    .map(|t| t.strip_suffix('\x01').unwrap_or(t));
                let (kind, text) = match ctcp {
                    Some(ctcp) if message.command == "PRIVMSG" => {
                        match ctcp.split_once(' ').unwrap_or((ctcp, "")) {
                            ("ACTION", action) => (LineType::Action, action),
                            _ => return None,
                        }
                    }
                    Some(_) => return None,
                    None if message.command == "NOTICE" => (LineType::Notice, text),
                    None => (LineType::Privmsg, text),
                };
                let from_server = source.is_none_or(|s| s.user.is_none() && s.host.is_none());
                let destination = if isupport.is_channel(target) {
                    Destination::Channel(target.to_owned())
                } else if from_server {
                    Destination::Server
                } else if mine {
                    Destination::Query(target.to_owned())
                } else {
                    Destination::Query(nick.to_owned())
                };
                let speaker = (!nick.is_empty()).then_some(nick);
                let mut line = Line::new(stamp, kind, &message.command, speaker, text.to_owned());
                line.highlight = !mine && mentions(text, me, casemap);
                (destination, line)
            }
            "JOIN" => (
                channel(0),
                event(
                    LineType::Join,
                    format!("\x02{nick}\x02 ({user}@{host}{STOP}) joined the channel"),
                ),
            ),
            "PART" => (
                channel(0),
                event(
                    LineType::Part,
                    format!(
                        "\x02{nick}\x02 ({user}@{host}{STOP}) left the channel{}",
                        reason(1)
                    ),
                ),
            ),
            "QUIT" => (
                Destination::Nick(nick.to_owned()),
                event(
                    LineType::Quit,
                    format!("\x02{nick}\x02 ({user}@{host}{STOP}) left IRC{}", reason(0)),
                ),
            ),
            "KICK" => (
                channel(0),
                event(
                    LineType::Kick,
                    format!(
                        "\x02{nick}\x02 kicked \x02{}\x02 from the channel ({}{STOP})",
                        param(1),
                        param(2)
                    ),
                ),
            ),
            "NICK" => {
                let text = if mine {
                    format!("You're now known as \x02{}\x02", param(0))
                } else {
                    format!("{nick} is now known as \x02{}\x02", param(0))
                };
                (
                    Destination::Nick(nick.to_owned()),
                    event(LineType::Nick, text),
                )
            }
            "TOPIC" => (
                channel(0),
                event(
                    LineType::Topic,
                    format!(
                        "\x02{nick}\x02 changed the topic to \x02{}{STOP}\x02",
                        param(1)
                    ),
                ),
            ),
            "MODE" => {
                let destination = if isupport.is_channel(param(0)) {
                    channel(0)
                } else {
                    Destination::Server
                };
                let text = format!("\x02{nick}\x02 sets mode \x02{}\x02", params_from(1));
                (destination, event(LineType::Mode, text))
            }
            "332" => (
                channel(1),
                event(
                    LineType::Topic,
                    format!("Topic is \x02{}{STOP}\x02", param(2)),
                ),
            ),
            "333" => {
                let setter = Source::parse(param(2)).nick;
                let at = param(3)
                    .parse()
                    .ok()
                    .and_then(|secs| Local.timestamp_opt(secs, 0).single())
                    .map_or_else(|| param(3).to_owned(), |t| t.format(LONG_DATE).to_string());
                (
                    channel(1),
                    event(LineType::Topic, format!("Set by \x02{setter}\x02 on {at}")),
                )
            }
            "324" => (
                channel(1),
                event(
                    LineType::Mode,
                    format!("Mode is \x02{}\x02", params_from(2)),
                ),
            ),
            numeric if is_numeric(numeric) => {
                let n: u16 = numeric.parse().expect("three digits");
                // Textual's error range (`receiveErrorNumericReply`), less
                // ERR_NOMOTD.
                let body = if (401..597).contains(&n) && n != 422 {
                    format!("Message({n}): {}", params_from(1))
                } else {
                    params_from(1)
                };
                (Destination::Server, event(LineType::Debug, body))
            }
            _ => return None,
        };
        line.from_self = mine;
        line.msgid = message.tag("msgid").map(str::to_owned);
        Some((destination, line))
    }

    /// The text a reader sees, without formatting: `<nick> text` for a
    /// message, `* nick text` for an action, `-nick- text` for a notice, and
    /// the body alone for an event.
    pub fn plain_text(&self) -> String {
        let body = body::strip(&self.body);
        match (&self.nickname, self.kind) {
            (Some(nick), LineType::Privmsg) => format!("<{nick}> {body}"),
            (Some(nick), LineType::Action) => format!("* {nick} {body}"),
            (Some(nick), LineType::Notice) => format!("-{nick}- {body}"),
            _ => body,
        }
    }

    /// The line's HTML, ready for `MessageBuffer.bufferElementAppend`.
    pub fn render(&self, style: &Style) -> String {
        let spoilers = self.kind.is_spoken();
        let fragment = |ctx: &Value| {
            style
                .render("formattedMessageFragment", ctx)
                .unwrap_or_default()
        };
        let formatted_message = body::render(&self.body, spoilers, &fragment);

        let mut ctx: BTreeMap<String, Value> = BTreeMap::new();
        let mut set = |key: &str, value: Value| {
            ctx.insert(key.to_owned(), value);
        };
        let timestamp_format = style
            .timestamp_format
            .as_deref()
            .unwrap_or(DEFAULT_TIMESTAMP_FORMAT);
        set("lineNumber", self.id.to_string().into());
        set("lineType", self.kind.as_str().into());
        set("command", self.command.clone().into());
        set("rawCommand", self.command.clone().into());
        let class = if self.kind.is_spoken() {
            "text"
        } else {
            "event"
        };
        set("lineClassAttribute", class.into());
        set(
            "highlightAttribute",
            if self.highlight { "true" } else { "false" }.into(),
        );
        set("isHighlight", self.highlight.into());
        set("isRemoteMessage", (!self.from_self).into());
        set(
            "timestamp",
            format!("{}", self.received_at.timestamp()).into(),
        );
        set(
            "formattedTimestamp",
            format_time(&self.received_at, timestamp_format).into(),
        );
        set(
            "localizedTimestamp",
            self.received_at.format(LONG_DATE).to_string().into(),
        );
        set("formattedMessage", formatted_message.into());
        set("message", self.body.clone().into());
        set("nicknameColorHashingEnabled", true.into());
        if let Some(nick) = &self.nickname {
            set("isNicknameAvailable", true.into());
            set("nickname", nick.clone().into());
            set(
                "nicknameType",
                if self.from_self { "myself" } else { "normal" }.into(),
            );
            let formatted = match self.kind {
                LineType::Action => format!("{nick} "),
                LineType::Notice => format!("-{nick}-"),
                _ => format_nickname(
                    style
                        .nickname_format
                        .as_deref()
                        .unwrap_or(DEFAULT_NICKNAME_FORMAT),
                    nick,
                    "",
                ),
            };
            set("formattedNickname", formatted.trim().to_owned().into());
            if matches!(self.kind, LineType::Privmsg | LineType::Action) {
                set(
                    "nicknameColorStyle",
                    nickname_color(nick, style.nickname_colors).into(),
                );
            }
        }
        let template = if self.kind.is_spoken() {
            "newMessagePostedWithSender"
        } else {
            "newMessagePostedWithoutSender"
        };
        style.render(template, &Value::Map(ctx)).unwrap_or_else(|| {
            eprintln!("textual-gtk: the style has no {template} template");
            String::new()
        })
    }
}

/// strftime that falls back to Textual's default when a style's format is
/// one chrono cannot render.
fn format_time(time: &DateTime<Local>, format: &str) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    if write!(out, "{}", time.format(format)).is_ok() {
        return out;
    }
    time.format(DEFAULT_TIMESTAMP_FORMAT).to_string()
}

fn is_numeric(command: &str) -> bool {
    command.len() == 3 && command.bytes().all(|b| b.is_ascii_digit())
}

/// Whether `text` names `nick` as a whole word (Textual's exact match: no
/// letter or digit on either side), with case folded by the server's
/// casemap.
fn mentions(text: &str, nick: &str, casemap: CaseMapping) -> bool {
    if nick.is_empty() {
        return false;
    }
    // Folding maps ASCII to ASCII and leaves the rest, so byte offsets in
    // the folded text are offsets in the original.
    let text = casemap.nick(text);
    let text = text.as_str();
    let nick = casemap.nick(nick);
    let nick = nick.as_str();
    let is_word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    text.match_indices(&nick).any(|(i, m)| {
        !is_word(text[..i].chars().next_back()) && !is_word(text[i + m.len()..].chars().next())
    })
}

/// Expands Textual's nickname format: `%n` the nick, `%@` the mode symbol,
/// `%%` a percent sign, `%Nn` the nick padded on the right to width N and
/// `%-Nn` padded on the left (`IRCClient.m:5200-5217`). Unknown markers
/// are dropped.
fn format_nickname(format: &str, nick: &str, mode: &str) -> String {
    let mut out = String::new();
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let left = chars.next_if_eq(&'-').is_some();
        let mut width = String::new();
        while let Some(d) = chars.next_if(char::is_ascii_digit) {
            width.push(d);
        }
        let width: usize = width.parse().unwrap_or(0);
        match chars.next() {
            Some('n') if left => out.push_str(&format!("{nick:>width$}")),
            Some('n') => out.push_str(&format!("{nick:<width$}")),
            Some('@') => out.push_str(mode),
            Some('%') => out.push('%'),
            Some(_) | None => {}
        }
    }
    out
}

/// Textual's `IRCUserNicknameColorStyleGenerator`: MD5 of `"a-" + nick`,
/// first four bytes as a little-endian integer, mapped into HSL ranges
/// tuned per background.
pub fn nickname_color(nick: &str, colors: NicknameColors) -> String {
    let digest = Md5::digest(format!("a-{}", nick.to_lowercase()).as_bytes());
    let hash = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]);
    let shash = hash >> 1;
    let lhash = hash >> 2;
    let h = hash % 360;
    let (s, l) = match colors {
        NicknameColors::HslLight => {
            let mut s = shash % 50 + 35;
            let mut l = lhash % 38 + 20;
            if h > 45 && h <= 195 {
                l = lhash % 21 + 20;
                s = if l > 31 {
                    shash % 40 + 55
                } else {
                    shash % 35 + 65
                };
            }
            if h <= 25 || h >= 335 {
                s = shash % 33 + 45;
            }
            (s, l)
        }
        NicknameColors::HslDark => {
            let mut s = shash % 50 + 45;
            let mut l = lhash % 36 + 45;
            if (280..335).contains(&h) {
                l = lhash % 36 + 50;
            }
            if (210..240).contains(&h) {
                l = lhash % 30 + 60;
            }
            if (240..280).contains(&h) {
                s = shash % 55 + 40;
                l = lhash % 20 + 65;
            }
            if h <= 25 || h >= 335 {
                s = shash % 33 + 45;
            }
            if (50..=150).contains(&h) {
                s = shash % 50 + 40;
            }
            (s, l)
        }
    };
    format!("hsl({h},{s}%,{l}%)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nickname_format_expands_like_textual() {
        assert_eq!(format_nickname("<%@%n>", "alice", "@"), "<@alice>");
        assert_eq!(format_nickname("%8n|", "bob", ""), "bob     |");
        assert_eq!(format_nickname("%-8n|", "bob", ""), "     bob|");
        assert_eq!(format_nickname("100%% %n%q%", "x", ""), "100% x");
    }

    #[test]
    fn a_bad_style_timestamp_format_falls_back() {
        let time = Local::now();
        assert_eq!(
            format_time(&time, "[%Q %H]"),
            format_time(&time, DEFAULT_TIMESTAMP_FORMAT)
        );
    }

    #[test]
    fn mentions_need_a_whole_word_under_the_casemap() {
        let rfc = CaseMapping::Rfc1459;
        assert!(mentions("hey Tester, look", "tester", rfc));
        assert!(!mentions("testers unite", "tester", rfc));
        assert!(!mentions("anything", "", rfc));
        assert!(mentions("ping NICK{}: there", "nick[]", rfc));
        assert!(!mentions("ping nick{}", "nick[]", CaseMapping::Ascii));
        assert!(mentions("é-nick", "nick", rfc), "punctuation is a boundary");
    }

    #[test]
    fn nickname_color_is_stable_and_case_blind() {
        let a = nickname_color("Alice", NicknameColors::HslLight);
        assert_eq!(a, nickname_color("alice", NicknameColors::HslLight));
        assert!(a.starts_with("hsl("), "{a}");
        assert_ne!(a, nickname_color("bob", NicknameColors::HslLight));
    }

    fn stamp() -> Stamp {
        Stamp {
            id: LineId::new(1),
            at: Local.timestamp_opt(0, 0).unwrap(),
        }
    }

    fn ergo() -> Isupport {
        let mut isupport = Isupport::default();
        isupport.apply(&["CHANTYPES=#", "PREFIX=(qaohv)~&@%+", "STATUSMSG=~&@%+"]);
        isupport
    }

    #[test]
    fn maps_server_messages_to_lines() {
        let isupport = ergo();
        let line =
            |raw: &str| Line::from_message(&Message::parse(raw).unwrap(), "me", &isupport, stamp());
        let (dest, l) = line(":alice!a@h PRIVMSG #c :hi me").unwrap();
        assert_eq!(dest, Destination::Channel("#c".into()));
        assert_eq!((l.kind, l.highlight), (LineType::Privmsg, true));
        assert_eq!(l.plain_text(), "<alice> hi me");

        let (_, l) = line(":alice!a@h PRIVMSG #c :\x01ACTION waves\x01").unwrap();
        assert_eq!((l.kind, l.body.as_str()), (LineType::Action, "waves"));

        let (dest, l) = line(":alice!a@h QUIT :bye").unwrap();
        assert_eq!(dest, Destination::Nick("alice".into()));
        assert_eq!(l.body, "\x02alice\x02 (a@h\x0F) left IRC (bye\x0F)");
        assert_eq!(l.plain_text(), "alice (a@h) left IRC (bye)");

        let (dest, _) = line(":bob!b@h PRIVMSG me :psst").unwrap();
        assert_eq!(dest, Destination::Query("bob".into()));
        assert!(line("PING :x").is_none());

        let (dest, l) = line("NOTICE AUTH :*** Looking up your hostname").unwrap();
        assert_eq!((dest, l.nickname), (Destination::Server, None));
        let (dest, _) = line(":irc.example NOTICE me :hi").unwrap();
        assert_eq!(dest, Destination::Server);
        let (dest, l) = line("@msgid=x1 :ME!u@h PRIVMSG bob :echoed").unwrap();
        assert_eq!(dest, Destination::Query("bob".into()));
        assert!(l.from_self && !l.highlight);
        assert_eq!(l.msgid.as_deref(), Some("x1"));
        let (dest, _) = line(":a!u@h PRIVMSG @#c :ops only").unwrap();
        assert_eq!(dest, Destination::Channel("#c".into()));
        let (dest, _) = line(":a!u@h PRIVMSG &#c :admins only").unwrap();
        assert_eq!(dest, Destination::Channel("#c".into()));

        assert!(line(":a!u@h PRIVMSG #c :\x01VERSION\x01").is_none());
        let (_, l) = line(":a!u@h PRIVMSG #c :\x01ACTION\x01").unwrap();
        assert_eq!((l.kind, l.body.as_str()), (LineType::Action, ""));
        assert!(line(":a!u@h NOTICE #c :\x01ACTION x\x01").is_none());
    }

    #[test]
    fn numerics_print_like_textual() {
        let isupport = ergo();
        let line = |raw: &str| {
            let (dest, l) =
                Line::from_message(&Message::parse(raw).unwrap(), "me", &isupport, stamp())
                    .unwrap();
            (dest, l.plain_text())
        };
        assert_eq!(
            line(":s 001 me :Welcome to IRC"),
            (Destination::Server, "Welcome to IRC".into())
        );
        assert_eq!(
            line(":s 403 me #x :No such channel"),
            (
                Destination::Server,
                "Message(403): #x No such channel".into()
            )
        );
        assert_eq!(
            line(":s 422 me :MOTD File is missing"),
            (Destination::Server, "MOTD File is missing".into())
        );
        assert_eq!(
            line(":s 332 me #c :the topic"),
            (
                Destination::Channel("#c".into()),
                "Topic is the topic".into()
            )
        );
        assert_eq!(
            line(":s 324 me #c +Cnt"),
            (Destination::Channel("#c".into()), "Mode is +Cnt".into())
        );
        assert_eq!(
            line(":me!u@h MODE me +i").0,
            Destination::Server,
            "a user mode goes to the console"
        );
    }
}
