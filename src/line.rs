//! One printed line of a chat view (Textual's `TVCLogLine`), built from an
//! IRC message and rendered through the style's line template.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Local};
use md5::{Digest, Md5};

use crate::irc::Message;
use crate::style::template::Value;
use crate::style::{NicknameColors, Style, body};

const DEFAULT_NICKNAME_FORMAT: &str = "<%@%n>";
const DEFAULT_TIMESTAMP_FORMAT: &str = "[%H:%M:%S]";
/// `\x0F`, which Textual appends to user-supplied text inside its own
/// formatted strings so that text's formatting cannot leak out.
const STOP: &str = "\x0F";

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
    fn is_spoken(self) -> bool {
        matches!(
            self,
            LineType::Privmsg | LineType::Action | LineType::Notice
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub id: String,
    pub received_at: DateTime<Local>,
    pub kind: LineType,
    /// The IRC command or numeric the line came from.
    pub command: String,
    /// Who spoke, for spoken lines.
    pub nickname: Option<String>,
    pub from_self: bool,
    /// Body text, still carrying IRC formatting codes.
    pub body: String,
    pub highlight: bool,
}

/// Where a line belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    Channel(String),
    /// Every channel this nick shares with us (QUIT and NICK).
    ChannelsOf(String),
    Server,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Unique within the process, shaped like Textual's 17-character ids.
fn next_id() -> String {
    let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("{:04X}-{:012X}", std::process::id() & 0xFFFF, n)
}

impl Line {
    pub fn new(kind: LineType, command: &str, nickname: Option<&str>, body: String) -> Self {
        Line {
            id: next_id(),
            received_at: Local::now(),
            kind,
            command: command.to_owned(),
            nickname: nickname.map(str::to_owned),
            from_self: false,
            body,
            highlight: false,
        }
    }

    /// The line a server message prints, or `None` for messages that print
    /// nothing yet.
    pub fn from_message(message: &Message, own_nick: &str) -> Option<(Destination, Line)> {
        let source = message.source();
        let nick = source.map_or("", |s| s.nick);
        let user = source.and_then(|s| s.user).unwrap_or("");
        let host = source.and_then(|s| s.host).unwrap_or("");
        let param = |i| message.param(i).unwrap_or("");
        let reason = |i| {
            message
                .param(i)
                .filter(|r| !r.is_empty())
                .map(|r| format!(" ({r}{STOP})"))
                .unwrap_or_default()
        };
        let mine = nick.eq_ignore_ascii_case(own_nick);
        let channel = || Destination::Channel(param(0).to_owned());

        let (destination, mut line) = match message.command.as_str() {
            "PRIVMSG" | "NOTICE" => {
                // STATUSMSG (`@#chan`) goes to the channel itself; `+chan`
                // is a channel in its own right.
                let stripped = param(0).trim_start_matches(['@', '+', '%', '~']);
                let target = if stripped.starts_with(['#', '&', '!']) {
                    stripped
                } else {
                    param(0)
                };
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
                let destination = if target.starts_with(['#', '&', '!', '+']) {
                    Destination::Channel(target.to_owned())
                } else if from_server {
                    Destination::Server
                } else if mine {
                    Destination::Channel(target.to_owned())
                } else {
                    Destination::Channel(nick.to_owned())
                };
                let speaker = (!nick.is_empty()).then_some(nick);
                let mut line = Line::new(kind, &message.command, speaker, text.to_owned());
                line.highlight = !mine && mentions(text, own_nick);
                (destination, line)
            }
            "JOIN" => (
                channel(),
                event(
                    LineType::Join,
                    message,
                    format!("\x02{nick}\x02 ({user}@{host}{STOP}) joined the channel"),
                ),
            ),
            "PART" => (
                channel(),
                event(
                    LineType::Part,
                    message,
                    format!(
                        "\x02{nick}\x02 ({user}@{host}{STOP}) left the channel{}",
                        reason(1)
                    ),
                ),
            ),
            "QUIT" => (
                Destination::ChannelsOf(nick.to_owned()),
                event(
                    LineType::Quit,
                    message,
                    format!("\x02{nick}\x02 ({user}@{host}{STOP}) left IRC{}", reason(0)),
                ),
            ),
            "KICK" => (
                channel(),
                event(
                    LineType::Kick,
                    message,
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
                    Destination::ChannelsOf(nick.to_owned()),
                    event(LineType::Nick, message, text),
                )
            }
            "TOPIC" => (
                channel(),
                event(
                    LineType::Topic,
                    message,
                    format!(
                        "\x02{nick}\x02 changed the topic to \x02{}{STOP}\x02",
                        param(1)
                    ),
                ),
            ),
            "MODE" if param(0).starts_with(['#', '&', '!', '+']) => {
                let modes = message.params[1..].join(" ");
                (
                    channel(),
                    event(
                        LineType::Mode,
                        message,
                        format!("\x02{nick}\x02 sets mode \x02{modes}\x02"),
                    ),
                )
            }
            _ => return None,
        };
        line.from_self = mine;
        Some((destination, line))
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
        set("lineNumber", self.id.clone().into());
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
            self.received_at
                .format("%B %-d, %Y at %-I:%M:%S %p")
                .to_string()
                .into(),
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

fn event(kind: LineType, message: &Message, body: String) -> Line {
    Line::new(kind, &message.command, None, body)
}

/// Whether `text` names `nick` as a whole word, ignoring case.
fn mentions(text: &str, nick: &str) -> bool {
    if nick.is_empty() {
        return false;
    }
    let text = text.to_lowercase();
    let nick = nick.to_lowercase();
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
    fn mentions_need_a_whole_word() {
        assert!(mentions("hey Tester, look", "tester"));
        assert!(!mentions("testers unite", "tester"));
        assert!(!mentions("anything", ""));
    }

    #[test]
    fn nickname_color_is_stable_and_case_blind() {
        let a = nickname_color("Alice", NicknameColors::HslLight);
        assert_eq!(a, nickname_color("alice", NicknameColors::HslLight));
        assert!(a.starts_with("hsl("), "{a}");
        assert_ne!(a, nickname_color("bob", NicknameColors::HslLight));
    }

    #[test]
    fn maps_server_messages_to_lines() {
        let line = |raw: &str| Line::from_message(&Message::parse(raw).unwrap(), "me");
        let (dest, l) = line(":alice!a@h PRIVMSG #c :hi me").unwrap();
        assert_eq!(dest, Destination::Channel("#c".into()));
        assert_eq!((l.kind, l.highlight), (LineType::Privmsg, true));

        let (_, l) = line(":alice!a@h PRIVMSG #c :\x01ACTION waves\x01").unwrap();
        assert_eq!((l.kind, l.body.as_str()), (LineType::Action, "waves"));

        let (dest, l) = line(":alice!a@h QUIT :bye").unwrap();
        assert_eq!(dest, Destination::ChannelsOf("alice".into()));
        assert_eq!(l.body, "\x02alice\x02 (a@h\x0F) left IRC (bye\x0F)");

        let (dest, _) = line(":bob!b@h PRIVMSG me :psst").unwrap();
        assert_eq!(dest, Destination::Channel("bob".into()));
        assert!(line("PING :x").is_none());

        let (dest, l) = line("NOTICE AUTH :*** Looking up your hostname").unwrap();
        assert_eq!((dest, l.nickname), (Destination::Server, None));
        let (dest, _) = line(":irc.example NOTICE me :hi").unwrap();
        assert_eq!(dest, Destination::Server);
        let (dest, _) = line(":me!u@h PRIVMSG bob :echoed").unwrap();
        assert_eq!(dest, Destination::Channel("bob".into()));
        let (dest, _) = line(":a!u@h PRIVMSG @#c :ops only").unwrap();
        assert_eq!(dest, Destination::Channel("#c".into()));
        let (dest, _) = line(":a!u@h PRIVMSG +modeless :hi").unwrap();
        assert_eq!(dest, Destination::Channel("+modeless".into()));

        assert!(line(":a!u@h PRIVMSG #c :\x01VERSION\x01").is_none());
        let (_, l) = line(":a!u@h PRIVMSG #c :\x01ACTION\x01").unwrap();
        assert_eq!((l.kind, l.body.as_str()), (LineType::Action, ""));
        assert!(line(":a!u@h NOTICE #c :\x01ACTION x\x01").is_none());
    }
}
