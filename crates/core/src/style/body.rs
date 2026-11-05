//! Turns a message body with IRC formatting codes into HTML, one
//! `formattedMessageFragment` render per run of identical formatting, the way
//! Textual's `TVCLogRenderer` does. Link, channel and nickname tagging are not
//! done yet.

use std::collections::BTreeMap;
use std::ops::Range;

use super::template::Value;

const BOLD: char = '\x02';
const COLOR_DIGIT: char = '\x03';
const COLOR_HEX: char = '\x04';
const RESET: char = '\x0F';
const MONOSPACE: char = '\x11';
const ITALIC_OLD: char = '\x16';
const ITALIC: char = '\x1D';
const STRIKETHROUGH: char = '\x1E';
const UNDERLINE: char = '\x1F';
const HIGHEST_COLOR_DIGIT: u8 = 98;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Color {
    /// An mIRC colour number, styled by `baseLayout.css`.
    Digit(u8),
    /// `rrggbb`, set inline.
    Hex(String),
}

impl Color {
    fn value(&self) -> String {
        match self {
            Color::Digit(n) => n.to_string(),
            Color::Hex(hex) => format!("#{hex}"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Effects {
    bold: bool,
    italic: bool,
    monospace: bool,
    strikethrough: bool,
    underline: bool,
    foreground: Option<Color>,
    background: Option<Color>,
}

#[derive(Debug, PartialEq, Eq)]
struct Run {
    text: String,
    effects: Effects,
}

fn parse_runs(body: &str) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut effects = Effects::default();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            BOLD => effects.bold = !effects.bold,
            ITALIC | ITALIC_OLD => effects.italic = !effects.italic,
            MONOSPACE => effects.monospace = !effects.monospace,
            STRIKETHROUGH => effects.strikethrough = !effects.strikethrough,
            UNDERLINE => effects.underline = !effects.underline,
            RESET => effects = Effects::default(),
            COLOR_DIGIT | COLOR_HEX => {
                let (fg, bg) = if c == COLOR_DIGIT {
                    take_digit_colors(&mut chars)
                } else {
                    take_hex_colors(&mut chars)
                };
                // A colour code without a foreground ends the colour; the
                // background survives only when a new foreground is given.
                if bg.is_some() {
                    effects.background = bg;
                } else if fg.is_none() {
                    effects.background = None;
                }
                effects.foreground = fg;
            }
            c => match runs.last_mut() {
                Some(run) if run.effects == effects => run.text.push(c),
                _ => runs.push(Run {
                    text: c.to_string(),
                    effects: effects.clone(),
                }),
            },
        }
    }
    runs
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

fn take_digits(chars: &mut Chars<'_>, max: usize) -> Option<u8> {
    let mut value: Option<u8> = None;
    for _ in 0..max {
        match chars.peek().and_then(|c| c.to_digit(10)) {
            Some(d) => {
                value = Some(value.unwrap_or(0) * 10 + d as u8);
                chars.next();
            }
            None => break,
        }
    }
    value
}

/// `\x03` then `F[F][,B[B]]`. A comma not followed by a digit is left in
/// the text. Numbers above 98 are consumed but ignored.
fn take_digit_colors(chars: &mut Chars<'_>) -> (Option<Color>, Option<Color>) {
    let valid = |n: u8| (n <= HIGHEST_COLOR_DIGIT).then_some(Color::Digit(n));
    let Some(fg) = take_digits(chars, 2) else {
        return (None, None);
    };
    let mut lookahead = chars.clone();
    let bg = if lookahead.next() == Some(',') && lookahead.peek().is_some_and(char::is_ascii_digit)
    {
        chars.next();
        take_digits(chars, 2)
    } else {
        None
    };
    (valid(fg), bg.and_then(valid))
}

/// `\x04` then `RRGGBB[,RRGGBB]`.
fn take_hex_colors(chars: &mut Chars<'_>) -> (Option<Color>, Option<Color>) {
    fn take_hex(chars: &mut Chars<'_>) -> Option<String> {
        let candidate: String = chars.clone().take(6).collect();
        if candidate.len() == 6 && candidate.chars().all(|c| c.is_ascii_hexdigit()) {
            chars.nth(5);
            Some(candidate.to_ascii_uppercase())
        } else {
            None
        }
    }
    let Some(fg) = take_hex(chars) else {
        return (None, None);
    };
    let mut lookahead = chars.clone();
    let bg = if lookahead.next() == Some(',') {
        let bg = take_hex(&mut lookahead);
        if bg.is_some() {
            *chars = lookahead;
        }
        bg
    } else {
        None
    };
    (Some(Color::Hex(fg)), bg.map(Color::Hex))
}

/// Textual's `escapeString`: HTML escaping, a tab as four spaces and runs
/// of two spaces kept, plus a leading or trailing space kept.
fn escape_fragment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\t' => out.push_str("&nbsp;&nbsp;&nbsp;&nbsp;"),
            c => out.push(c),
        }
    }
    let mut out = out.replace("  ", "&nbsp;&nbsp;");
    if out.starts_with(' ') {
        out.replace_range(..1, "&nbsp;");
    }
    if out.ends_with(' ') {
        let end = out.len() - 1;
        out.replace_range(end.., "&nbsp;");
    }
    out
}

/// The body's text with every formatting code removed.
pub fn strip(body: &str) -> String {
    parse_runs(body).into_iter().map(|run| run.text).collect()
}

/// Byte ranges of the links in `text`, which has no formatting codes. A
/// link is `scheme://` or `www.` up to the next whitespace, a stand-in
/// for Textual's `TLOLinkParser` until links are rendered.
pub fn links(text: &str) -> Vec<Range<usize>> {
    text.split(char::is_whitespace)
        .filter_map(|token| {
            let start = token.as_ptr() as usize - text.as_ptr() as usize;
            let lower = token.to_ascii_lowercase();
            let scheme = lower.find("://").and_then(|at| {
                let letters = lower[..at]
                    .bytes()
                    .rev()
                    .take_while(u8::is_ascii_alphabetic)
                    .count();
                (letters > 0).then_some(at - letters)
            });
            let www = || {
                lower.match_indices("www.").map(|(at, _)| at).find(|&at| {
                    !lower[..at]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_alphanumeric)
                })
            };
            let from = scheme.or_else(www)?;
            Some(start + from..start + token.len())
        })
        .collect()
}

/// Renders `body` to HTML. `spoilers` marks a run whose foreground and
/// background match (Textual does this for PRIVMSG, ACTION and NOTICE).
pub fn render(body: &str, spoilers: bool, fragment: &dyn Fn(&Value) -> String) -> String {
    let runs = parse_runs(body);
    let mut out = String::new();
    let mut open = Effects::default();
    let mut color_open = false;
    for (i, run) in runs.iter().enumerate() {
        let first = i == 0;
        let last = i + 1 == runs.len();
        let mut tokens: BTreeMap<String, Value> = BTreeMap::new();
        let mut set = |key: &str, value: Value| {
            tokens.insert(key.to_owned(), value);
        };

        let toggles = [
            ("fragmentIsBold", run.effects.bold, &mut open.bold),
            ("fragmentIsItalicized", run.effects.italic, &mut open.italic),
            (
                "fragmentIsMonospace",
                run.effects.monospace,
                &mut open.monospace,
            ),
            (
                "fragmentIsStruckthrough",
                run.effects.strikethrough,
                &mut open.strikethrough,
            ),
            (
                "fragmentIsUnderlined",
                run.effects.underline,
                &mut open.underline,
            ),
        ];
        for (name, on, was_open) in toggles {
            if on {
                if !*was_open {
                    *was_open = true;
                    set(&format!("{name}Opened"), true.into());
                }
                if last {
                    set(&format!("{name}ClosedAtEnd"), true.into());
                }
            } else if *was_open {
                *was_open = false;
                set(&format!("{name}ClosedAtStart"), true.into());
            }
        }

        let (fg, bg) = (&run.effects.foreground, &run.effects.background);
        let changed = (fg, bg) != (&open.foreground, &open.background);
        if changed {
            if color_open && !first {
                set("fragmentTextColorClosedAtStart", true.into());
            }
            color_open = false;
            if fg.is_some() || bg.is_some() {
                set("fragmentTextColorOpened", true.into());
                color_open = true;
            }
            let mut uses_style_tag = false;
            if let Some(color) = fg {
                set("fragmentForegroundColor", color.value().into());
                set("fragmentForegroundColorIsSet", true.into());
                uses_style_tag = matches!(color, Color::Hex(_));
            }
            if let Some(color) = bg {
                set("fragmentBackgroundColor", color.value().into());
                set("fragmentBackgroundColorIsSet", true.into());
                uses_style_tag = matches!(color, Color::Hex(_));
            }
            set("fragmentTextColorUsesStyleTag", uses_style_tag.into());
            let spoiler = spoilers && fg.is_some() && fg == bg;
            set("fragmentIsSpoiler", spoiler.into());
            open.foreground = fg.clone();
            open.background = bg.clone();
        }
        if last && color_open {
            set("fragmentTextColorClosedAtEnd", true.into());
        }

        set("messageFragment", escape_fragment(&run.text).into());
        out.push_str(&fragment(&Value::Map(tokens)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(body: &str) -> Vec<(String, Effects)> {
        parse_runs(body)
            .into_iter()
            .map(|r| (r.text, r.effects))
            .collect()
    }

    #[test]
    fn plain_text_is_one_run() {
        assert_eq!(runs("hello"), [("hello".into(), Effects::default())]);
    }

    #[test]
    fn toggles_and_reset_split_runs() {
        let got = runs("a\x02b\x1Dc\x0Fd");
        let bold = Effects {
            bold: true,
            ..Default::default()
        };
        let bold_italic = Effects {
            italic: true,
            ..bold.clone()
        };
        assert_eq!(
            got,
            [
                ("a".into(), Effects::default()),
                ("b".into(), bold),
                ("c".into(), bold_italic),
                ("d".into(), Effects::default()),
            ]
        );
    }

    #[test]
    fn digit_colors_follow_textual_rules() {
        let fg = |s: &str| runs(s)[0].1.foreground.clone();
        let bg = |s: &str| runs(s)[0].1.background.clone();
        assert_eq!(fg("\x034x"), Some(Color::Digit(4)));
        assert_eq!(fg("\x0304x"), Some(Color::Digit(4)));
        assert_eq!(runs("\x0312345")[0].0, "345");
        assert_eq!(bg("\x034,12x"), Some(Color::Digit(12)));
        assert_eq!(runs("\x034,x")[0].0, ",x", "comma without digit stays");
        assert_eq!(fg("\x0399x"), None, "99 is above the highest colour");
        let ended = runs("\x034,2a\x03b");
        assert_eq!(ended[1].1, Effects::default(), "bare colour code ends both");
        let kept = runs("\x034,2a\x035b");
        assert_eq!(kept[1].1.background, Some(Color::Digit(2)));
    }

    #[test]
    fn hex_colors_parse_six_digits() {
        let got = runs("\x04FF0000,00ff00x");
        assert_eq!(got[0].1.foreground, Some(Color::Hex("FF0000".into())));
        assert_eq!(got[0].1.background, Some(Color::Hex("00FF00".into())));
        assert_eq!(runs("\x04FF00x")[0].0, "FF00x");
    }

    #[test]
    fn escapes_like_textual() {
        assert_eq!(
            escape_fragment("<a> & \"b\""),
            "&lt;a&gt; &amp; &quot;b&quot;"
        );
        assert_eq!(
            escape_fragment(" two  spaces "),
            "&nbsp;two&nbsp;&nbsp;spaces&nbsp;"
        );
        assert_eq!(escape_fragment("\t"), "&nbsp;&nbsp;&nbsp;&nbsp;");
    }

    #[test]
    fn open_and_close_flags_bracket_each_effect() {
        let seen = std::cell::RefCell::new(Vec::new());
        let capture = |ctx: &Value| {
            let Value::Map(map) = ctx else { unreachable!() };
            seen.borrow_mut()
                .push(map.keys().cloned().collect::<Vec<_>>().join(","));
            String::new()
        };
        render("a\x02b\x02c\x034d", false, &capture);
        let seen = seen.into_inner();
        assert_eq!(seen.len(), 4);
        assert!(seen[1].contains("fragmentIsBoldOpened"), "{}", seen[1]);
        assert!(
            seen[2].contains("fragmentIsBoldClosedAtStart"),
            "{}",
            seen[2]
        );
        assert!(seen[3].contains("fragmentTextColorOpened"), "{}", seen[3]);
        assert!(
            seen[3].contains("fragmentTextColorClosedAtEnd"),
            "{}",
            seen[3]
        );
    }
}
