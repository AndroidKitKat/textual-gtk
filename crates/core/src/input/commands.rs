//! The commands the input understands, in one table that both dispatch
//! and Tab completion read, and Textual's rules for turning a typed line
//! into a command or a message (`IRCClient inputText:asCommand:`,
//! `IRCClient.m:2721-2803`).

/// What a command does; the workspace runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Away,
    Close,
    Conn,
    Invite,
    Join,
    Kick,
    Me,
    Mode,
    Msg,
    Nick,
    Notice,
    Part,
    Query,
    Quit,
    Raw,
    Topic,
    Whois,
}

pub struct Spec {
    pub name: &'static str,
    pub verb: Verb,
    /// Textual's `IRCCommandIndexLocalData.plist` arguments, for its
    /// "Invalid syntax" line.
    pub usage: &'static str,
}

/// Sorted by name. Aliases are Textual's (`j`, `leave`, `m`, `t`, `quote`,
/// `remove`).
pub const COMMANDS: &[Spec] = &[
    spec("away", Verb::Away, "[comment]"),
    spec("close", Verb::Close, "[target]"),
    spec("conn", Verb::Conn, "[server]"),
    spec("invite", Verb::Invite, "<nickname> [channel]"),
    spec("j", Verb::Join, "<channel[,channel]]> [key[,key]]"),
    spec("join", Verb::Join, "<channel[,channel]]> [key[,key]]"),
    spec("kick", Verb::Kick, "[channel] <nickname> [comment]"),
    spec("leave", Verb::Part, "[comment]"),
    spec("m", Verb::Mode, "<target> [flags] [arguments]"),
    spec("me", Verb::Me, "<message>"),
    spec("mode", Verb::Mode, "<target> [flags] [arguments]"),
    spec("msg", Verb::Msg, "<target[,target]]> <message>"),
    spec("nick", Verb::Nick, "<nickname>"),
    spec("notice", Verb::Notice, "<target[,target]]> <message>"),
    spec("part", Verb::Part, "[comment]"),
    spec("query", Verb::Query, "<nickname>"),
    spec("quit", Verb::Quit, "[comment]"),
    spec("quote", Verb::Raw, "<input>"),
    spec("raw", Verb::Raw, "<input>"),
    spec("remove", Verb::Close, "[target]"),
    spec("t", Verb::Topic, "<channel> [topic]"),
    spec("topic", Verb::Topic, "<channel> [topic]"),
    spec("whois", Verb::Whois, "<nickname>"),
];

const fn spec(name: &'static str, verb: Verb, usage: &'static str) -> Spec {
    Spec { name, verb, usage }
}

/// The table entry for a typed command name, ignoring case.
pub fn find(name: &str) -> Option<&'static Spec> {
    let name = name.to_lowercase();
    COMMANDS.iter().find(|s| s.name == name)
}

/// Textual's "Invalid syntax" line for `typed` (`IRC[atq-93]`).
pub fn invalid_syntax(typed: &str, spec: &Spec) -> String {
    let usage = if spec.usage.is_empty() {
        typed.to_uppercase()
    } else {
        format!("{} {}", typed.to_uppercase(), spec.usage)
    };
    format!("Invalid syntax: \u{201C}{usage}\u{201C}")
}

/// One typed line, after Textual's slash rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typed<'a> {
    /// A command line without its slash: `join #a`.
    Command(&'a str),
    Text(&'a str),
}

/// On a console every line is a command, with or without a slash. In a
/// channel or query `/x` is a command, `//x` sends `/x`, and a lone `/` is
/// text.
pub fn classify(line: &str, console: bool) -> Typed<'_> {
    if console {
        return Typed::Command(line.strip_prefix('/').unwrap_or(line));
    }
    match line.strip_prefix('/') {
        Some(rest) if !rest.is_empty() && !rest.starts_with('/') => Typed::Command(rest),
        Some(rest) if !rest.is_empty() => Typed::Text(rest),
        _ => Typed::Text(line),
    }
}

/// The first word and what follows the spaces after it, as Textual's
/// `getTokenAsString`: a line that starts with a space has an empty first
/// word.
pub fn token(s: &str) -> (&str, &str) {
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], s[i..].trim_start()),
        None => (s, ""),
    }
}

/// Textual's `splitIntoLines`: every line break ends a line, so `\r\n`
/// leaves an empty line between; empty lines are kept and a final break
/// adds none.
pub fn split_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text
        .split(['\n', '\r', '\x0b', '\x0c', '\u{85}', '\u{2028}', '\u{2029}'])
        .collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// Textual's `TXMaximumIRCBodyLength`.
const BODY_MAX: usize = 510;

/// Whether Textual asks before sending: more than 4 lines, or more than
/// 4 × 510 UTF-16 units in all (`IRCClient.m:2759`).
pub fn is_flood(text: &str) -> bool {
    split_lines(text).len() > 4 || text.encode_utf16().count() > BODY_MAX * 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_finds_aliases_in_any_case() {
        let names: Vec<_> = COMMANDS.iter().map(|s| s.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert_eq!(find("J").map(|s| s.verb), Some(Verb::Join));
        assert_eq!(find("Quote").map(|s| s.verb), Some(Verb::Raw));
        assert!(find("names").is_none());
    }

    #[test]
    fn slash_rules() {
        assert_eq!(classify("/join #a", false), Typed::Command("join #a"));
        assert_eq!(classify("//me", false), Typed::Text("/me"));
        assert_eq!(classify("/", false), Typed::Text("/"));
        assert_eq!(classify("hello", false), Typed::Text("hello"));
        assert_eq!(classify("", false), Typed::Text(""));
        assert_eq!(classify("join #a", true), Typed::Command("join #a"));
        assert_eq!(classify("/join #a", true), Typed::Command("join #a"));
        assert_eq!(classify("//x", true), Typed::Command("/x"));
    }

    #[test]
    fn tokens_like_textual() {
        assert_eq!(token("#a  key rest"), ("#a", "key rest"));
        assert_eq!(token("#a"), ("#a", ""));
        assert_eq!(token(" #a"), ("", "#a"));
        assert_eq!(token(""), ("", ""));
    }

    #[test]
    fn lines_split_on_every_break() {
        assert_eq!(split_lines("a\nb"), ["a", "b"]);
        assert_eq!(split_lines("a\r\nb"), ["a", "", "b"]);
        assert_eq!(split_lines("a\n\nb\n"), ["a", "", "b"]);
        assert_eq!(split_lines("a"), ["a"]);
        assert!(split_lines("").is_empty());
    }

    #[test]
    fn flood_is_more_than_four_lines_or_2040_units() {
        assert!(!is_flood("1\n2\n3\n4"));
        assert!(is_flood("1\n2\n3\n4\n5"));
        assert!(!is_flood(&"x".repeat(2040)));
        assert!(is_flood(&"x".repeat(2041)));
        assert!(is_flood(&"😀".repeat(1021)));
    }

    #[test]
    fn invalid_syntax_quotes_the_usage() {
        let join = find("j").unwrap();
        assert_eq!(
            invalid_syntax("j", join),
            "Invalid syntax: \u{201C}J <channel[,channel]]> [key[,key]]\u{201C}"
        );
    }
}
