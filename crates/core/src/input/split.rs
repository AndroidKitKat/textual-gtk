//! Cuts typed text into messages that fit one IRC line each, as Textual's
//! `stringFormattedForChannel:onClient:withLineType:` does
//! (`IRCColorFormat.m:375-600`). The budget is what the server relays:
//! `:nick!user@host PRIVMSG #target :text\r\n` must fit 512 bytes, and
//! Ergo answers 417 to a longer line instead of truncating it.

/// The longest host the protocol allows, and the common USERLEN, assumed
/// until the server has shown us our own `user@host`.
const UNKNOWN_USER_HOST: usize = 10 + 1 + 63;
const LINE_MAX: usize = 512;
/// `\x01ACTION ` and the closing `\x01`.
const ACTION_WRAPPER: usize = 9;
/// Textual's `_textTruncationWrapMaxDistance`: how far back from the end
/// of a full chunk a space is looked for.
const WRAP_DISTANCE: usize = 25;

const BOLD: char = '\x02';
const COLOR_DIGIT: char = '\x03';
const COLOR_HEX: char = '\x04';
const RESET: char = '\x0F';
const MONOSPACE: char = '\x11';
const ITALIC_OLD: char = '\x16';
const ITALIC: char = '\x1D';
const STRIKETHROUGH: char = '\x1E';
const UNDERLINE: char = '\x1F';

/// Bytes the text of one message may take. `user_host` is our `user@host`
/// once known; `command` is `PRIVMSG` or `NOTICE`.
pub fn budget(
    nick: &str,
    user_host: Option<&str>,
    command: &str,
    target: &str,
    action: bool,
) -> usize {
    let prefix = 1 + nick.len() + 1 + user_host.map_or(UNKNOWN_USER_HOST, str::len);
    let around = prefix + 1 + command.len() + 1 + target.len() + 2 + 2;
    let wrapper = if action { ACTION_WRAPPER } else { 0 };
    LINE_MAX.saturating_sub(around + wrapper)
}

/// The messages `text` goes out as. Each fits `budget` bytes; a message
/// after the first starts with the codes that reopen the formatting still
/// active where the previous one ended. A cut never falls inside a UTF-8
/// sequence, a formatting code or a character with its combining marks,
/// and prefers the last space within the final 25 bytes, which then
/// starts the next message (as in Textual).
pub fn split_for_wire(text: &str, budget: usize) -> Vec<String> {
    pieces(text, budget)
        .into_iter()
        .map(|(reopen, part)| reopen + part)
        .collect()
}

/// Each message as (reopening codes, slice of the input).
fn pieces(text: &str, budget: usize) -> Vec<(String, &str)> {
    let mut out = Vec::new();
    let mut format = Format::default();
    let mut rest = text;
    while !rest.is_empty() {
        let mut reopen = if out.is_empty() {
            String::new()
        } else {
            format.reopen()
        };
        if reopen.ends_with(|c: char| c.is_ascii_digit() || c.is_ascii_hexdigit())
            && rest.starts_with(',')
        {
            // `\x0304` before `,5` would read as a background colour.
            reopen.push_str("\x02\x02");
        }
        let room = budget.saturating_sub(reopen.len());
        let cut = if rest.len() <= room {
            rest.len()
        } else {
            cut_point(rest, room)
        };
        let (part, tail) = rest.split_at(cut);
        format.apply(part);
        out.push((reopen, part));
        rest = tail;
    }
    out
}

/// Where to end a message that cannot take all of `rest`.
fn cut_point(rest: &str, room: usize) -> usize {
    let ends: Vec<usize> = token_ends(rest).take_while(|e| *e <= room).collect();
    let Some(&last) = ends.last() else {
        // Not even one token fits: send it alone rather than stall.
        return token_len(rest);
    };
    let window = last.saturating_sub(WRAP_DISTANCE);
    ends.iter()
        .rev()
        .copied()
        .take_while(|e| *e >= window)
        .find(|e| *e > 0 && *e < last && rest[*e..].starts_with(char::is_whitespace))
        .unwrap_or(last)
}

/// The byte offsets where each token of `text` ends: a formatting code
/// with its arguments, or a character with the marks that join it.
fn token_ends(text: &str) -> impl Iterator<Item = usize> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        if at >= text.len() {
            return None;
        }
        at += token_len(&text[at..]);
        Some(at)
    })
}

fn token_len(s: &str) -> usize {
    let mut chars = s.chars();
    let first = chars.next().expect("not empty");
    let rest = &s[first.len_utf8()..];
    match first {
        COLOR_DIGIT => 1 + digit_args(rest),
        COLOR_HEX => 1 + hex_args(rest),
        c if c.is_control() => c.len_utf8(),
        _ => {
            let mut len = first.len_utf8();
            let mut joined = first == '\u{200D}';
            for c in s[len..].chars() {
                if c.is_control() || !(joined || extends(c)) {
                    break;
                }
                joined = c == '\u{200D}';
                len += c.len_utf8();
            }
            len
        }
    }
}

/// Marks that belong to the character before them: combining marks,
/// variation selectors, the zero-width joiner and emoji modifiers. A
/// stand-in for Textual's composed character sequences.
fn extends(c: char) -> bool {
    matches!(c,
        '\u{0300}'..='\u{036F}'
        | '\u{1AB0}'..='\u{1AFF}'
        | '\u{1DC0}'..='\u{1DFF}'
        | '\u{20D0}'..='\u{20FF}'
        | '\u{FE00}'..='\u{FE0F}'
        | '\u{FE20}'..='\u{FE2F}'
        | '\u{200D}'
        | '\u{1F3FB}'..='\u{1F3FF}'
        | '\u{E0020}'..='\u{E007F}'
        | '\u{E0100}'..='\u{E01EF}')
}

/// Length of `F[F][,B[B]]` after `\x03`, as the renderer reads it.
fn digit_args(s: &str) -> usize {
    let b = s.as_bytes();
    let digits = |from: usize| {
        b[from..]
            .iter()
            .take(2)
            .take_while(|c| c.is_ascii_digit())
            .count()
    };
    let fg = digits(0);
    if fg == 0 {
        return 0;
    }
    match b.get(fg) {
        Some(b',') if b.get(fg + 1).is_some_and(u8::is_ascii_digit) => fg + 1 + digits(fg + 1),
        _ => fg,
    }
}

/// Length of `RRGGBB[,RRGGBB]` after `\x04`.
fn hex_args(s: &str) -> usize {
    let hex = |from: usize| {
        s.get(from..from + 6)
            .is_some_and(|h| h.bytes().all(|c| c.is_ascii_hexdigit()))
    };
    if !hex(0) {
        return 0;
    }
    if s.as_bytes().get(6) == Some(&b',') && hex(7) {
        13
    } else {
        6
    }
}

/// The formatting in effect, as the renderer reads it
/// (`style::body::parse_runs`), with each colour kept as the code kind
/// and argument that set it.
#[derive(Debug, Default, Clone)]
struct Format {
    bold: bool,
    italic: bool,
    monospace: bool,
    strikethrough: bool,
    underline: bool,
    foreground: Option<(char, String)>,
    background: Option<(char, String)>,
}

impl Format {
    fn apply(&mut self, text: &str) {
        let mut at = 0;
        while at < text.len() {
            let len = token_len(&text[at..]);
            let token = &text[at..at + len];
            at += len;
            let mut chars = token.chars();
            match chars.next() {
                Some(BOLD) => self.bold = !self.bold,
                Some(ITALIC | ITALIC_OLD) => self.italic = !self.italic,
                Some(MONOSPACE) => self.monospace = !self.monospace,
                Some(STRIKETHROUGH) => self.strikethrough = !self.strikethrough,
                Some(UNDERLINE) => self.underline = !self.underline,
                Some(RESET) => *self = Format::default(),
                Some(code @ (COLOR_DIGIT | COLOR_HEX)) => {
                    let args = chars.as_str();
                    let (fg, bg) = match args.split_once(',') {
                        Some((fg, bg)) => (fg, Some(bg)),
                        None => (args, None),
                    };
                    let color = |s: &str| -> Option<(char, String)> {
                        if code == COLOR_HEX {
                            return (!s.is_empty()).then(|| (code, s.to_ascii_uppercase()));
                        }
                        let n: u8 = s.parse().ok()?;
                        (n <= 98).then(|| (code, format!("{n:02}")))
                    };
                    let fg = color(fg);
                    let bg = bg.and_then(color);
                    // As the renderer: a background comes only with a
                    // foreground, and a code without one ends both.
                    if bg.is_some() {
                        self.background = bg;
                    } else if fg.is_none() {
                        self.background = None;
                    }
                    self.foreground = fg;
                }
                _ => {}
            }
        }
    }

    fn reopen(&self) -> String {
        let mut out = String::new();
        for (on, code) in [
            (self.bold, BOLD),
            (self.italic, ITALIC),
            (self.monospace, MONOSPACE),
            (self.strikethrough, STRIKETHROUGH),
            (self.underline, UNDERLINE),
        ] {
            if on {
                out.push(code);
            }
        }
        match (&self.foreground, &self.background) {
            (None, None) => {}
            (Some((code, fg)), None) => {
                out.push(*code);
                out.push_str(fg);
            }
            (Some((fc, fg)), Some((bc, bg))) if fc == bc => {
                out.push_str(&format!("{fc}{fg},{bg}"));
            }
            // A background set by the other kind of code, or one left
            // after an out-of-range foreground: set it with a placeholder
            // foreground first, then the real foreground, which keeps it.
            (fg, Some((bc, bg))) => {
                let placeholder = if *bc == COLOR_DIGIT { "99" } else { "000000" };
                out.push_str(&format!("{bc}{placeholder},{bg}"));
                if let Some((fc, fg)) = fg {
                    out.push_str(&format!("{fc}{fg}"));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::body;

    #[test]
    fn budget_counts_the_relayed_prefix() {
        // ":n!u@h PRIVMSG #c :" is 19 bytes, plus CRLF.
        assert_eq!(
            budget("n", Some("u@h"), "PRIVMSG", "#c", false),
            512 - 19 - 2
        );
        assert_eq!(
            budget("n", Some("u@h"), "PRIVMSG", "#c", true),
            512 - 19 - 2 - 9
        );
        assert_eq!(
            budget("n", None, "PRIVMSG", "#c", false),
            512 - (1 + 1 + 1 + 74 + 1 + 7 + 1 + 2 + 2) - 2
        );
    }

    #[test]
    fn short_text_is_one_message() {
        assert_eq!(split_for_wire("hello", 100), ["hello"]);
        assert_eq!(split_for_wire("", 100), Vec::<String>::new());
    }

    #[test]
    fn a_space_near_the_end_wins_and_starts_the_next_message() {
        let text = format!("{} {}", "a".repeat(90), "b".repeat(20));
        assert_eq!(
            split_for_wire(&text, 100),
            ["a".repeat(90), format!(" {}", "b".repeat(20))]
        );
        // A space further back than 25 bytes is not used.
        let text = format!("{} {}", "a".repeat(10), "b".repeat(200));
        let parts = split_for_wire(&text, 100);
        assert_eq!(parts[0].len(), 100);
    }

    #[test]
    fn multibyte_characters_are_never_cut() {
        let text = "é".repeat(80);
        let parts = split_for_wire(&text, 101);
        assert_eq!(parts[0].len(), 100);
        assert_eq!(parts.concat(), text);
        let flag = "👍🏽".repeat(30);
        for part in split_for_wire(&flag, 50) {
            assert!(part.starts_with('👍'), "{part:?}");
        }
    }

    #[test]
    fn colour_codes_stay_whole_and_reopen() {
        let text = format!("x\x0304,12{}", "y".repeat(40));
        let parts = split_for_wire(&text, 20);
        assert_eq!(parts[0], format!("x\x0304,12{}", "y".repeat(13)));
        assert_eq!(parts[1], format!("\x0304,12{}", "y".repeat(14)));
        let text = format!("\x02\x1Fbold{}", "z".repeat(30));
        let parts = split_for_wire(&text, 16);
        assert!(parts[1].starts_with("\x02\x1Fz"), "{:?}", parts[1]);
    }

    #[test]
    fn a_reopened_colour_before_a_comma_cannot_take_it_as_a_background() {
        let text = format!("\x034{},5", "a".repeat(10));
        let parts = split_for_wire(&text, 12);
        assert_eq!(
            parts,
            [
                format!("\x034{}", "a".repeat(10)),
                "\x0304\x02\x02,5".into()
            ]
        );
        assert_eq!(body::strip(&parts[1]), ",5");
    }

    /// xorshift64, so every run sees the same texts.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn merged(runs: Vec<body::Run>) -> Vec<body::Run> {
        let mut out: Vec<body::Run> = Vec::new();
        for run in runs {
            match out.last_mut() {
                Some(last) if last.effects == run.effects => last.text.push_str(&run.text),
                _ => out.push(run),
            }
        }
        out
    }

    fn text(rng: &mut Rng) -> String {
        let atoms = [
            "a",
            "word",
            " ",
            "  ",
            "é",
            "€",
            "👍🏽",
            "e\u{301}",
            "\u{200D}",
            "\x02",
            "\x0F",
            "\x1D",
            "\x16",
            "\x1F",
            "\x03",
            "\x034",
            "\x0312,5",
            "\x03,",
            ",",
            "7",
            "\x04FF00aa",
            "\x04112233,445566",
            "\x04zz",
            "日本語",
        ];
        let len = rng.next() % 400;
        (0..len)
            .map(|_| atoms[(rng.next() % atoms.len() as u64) as usize])
            .collect()
    }

    #[test]
    fn every_message_fits_and_the_text_comes_back_whole() {
        let mut rng = Rng(0x5eed_1234_abcd_0042);
        for _ in 0..3000 {
            let text = text(&mut rng);
            let budget = 40 + (rng.next() % 300) as usize;
            let pieces = pieces(&text, budget);
            for (reopen, part) in &pieces {
                assert!(
                    reopen.len() + part.len() <= budget,
                    "{budget}: {reopen:?}{part:?}"
                );
                assert!(!part.is_empty());
            }
            let joined: String = pieces.iter().map(|(_, p)| *p).collect();
            assert_eq!(joined, text);
            // What the reader sees: each message renders with the
            // formatting that was active where it starts.
            let seen: Vec<_> = pieces
                .iter()
                .flat_map(|(r, p)| body::parse_runs(&format!("{r}{p}")))
                .collect();
            assert!(
                merged(seen) == merged(body::parse_runs(&text)),
                "formatting differs: {text:?}"
            );
        }
    }
}
