//! Tab completion: Textual's `TLONicknameCompletionStatus`. Offsets are in
//! characters of the input field. A Tab right after a Tab, with the text
//! and caret as that Tab left them, steps through the same matches;
//! anything else starts a new scan.

use std::ops::Range;

/// Names Textual offers after the members of a channel (`performCompletion_step1`).
const STATIC_NAMES: [&str; 7] = [
    "NickServ", "RootServ", "OperServ", "HostServ", "ChanServ", "MemoServ", "Textual",
];

/// Characters trimmed off the front of a nick for its second spelling.
const TRIMMED: &[char] = &['^', '[', ']', '-', '_', '`', '{', '}', '\\'];

/// Textual's completion preferences, with Textual's defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// `Keyboard -> Tab Key Completion Suffix`: added after a nick at the
    /// start of the line. Unset by default.
    pub suffix: Option<String>,
    /// `Tab Completion -> Do Not Use Whitespace for Missing Completion Suffix`.
    pub no_whitespace: bool,
    /// `Tab Completion -> Completion Suffix Cut Forward Until Space`.
    pub cut_forward: bool,
}

/// A channel member as completion sees it, in member-list order.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub nick: String,
    /// Conversation weight, already decayed to now.
    pub weight: f64,
}

/// What the focused buffer offers.
#[derive(Debug, Clone)]
pub struct Source {
    /// Command names, lowercase.
    pub commands: Vec<String>,
    /// The focused channel or query first, then the network's others.
    pub channels: Vec<String>,
    pub network: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub text: String,
    pub caret: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Command,
    Channel,
    Nick,
}

/// One scan, kept for the next Tab.
#[derive(Debug, Clone)]
struct Scan {
    kind: Kind,
    /// The text being completed, without a leading `/` or `@`.
    pattern: String,
    /// `/` or `@`, put back in front of the result.
    prefix: Option<char>,
    /// Where the pattern sat, prefix included. Only `start` and the
    /// original length are used, as in Textual.
    pattern_at: Range<usize>,
    /// The text after the pattern that the result replaces.
    suffix_at: Range<usize>,
    /// What `suffix_at` held when the scan was made.
    old_suffix: String,
    at_start: bool,
    index: Option<usize>,
    /// The text and caret the last completion left.
    left: Option<(String, usize)>,
}

#[derive(Debug, Clone, Default)]
pub struct Completion {
    scan: Option<Scan>,
}

impl Completion {
    /// Forgets the scan, as sending a line does.
    pub fn clear(&mut self) {
        self.scan = None;
    }

    /// Completes at `selection` in `text`. `members` is asked only when a
    /// nick is being completed, since reading weights decays them; `None`
    /// means the buffer has no members to offer (the console).
    pub fn complete(
        &mut self,
        text: &str,
        selection: Range<usize>,
        backward: bool,
        source: &Source,
        members: impl FnOnce() -> Option<Vec<Member>>,
        settings: &Settings,
    ) -> Option<Edit> {
        let chars: Vec<char> = text.chars().collect();
        let end = selection.end.min(chars.len());
        let selection = selection.start.min(end)..end;
        let reuse = self.scan.as_ref().is_some_and(|s| {
            s.index.is_some()
                && s.left
                    .as_ref()
                    .is_some_and(|(t, c)| t == text && selection == (*c..*c))
        });
        if !reuse {
            self.scan = Some(Scan::new(&chars, selection, settings));
        }
        let scan = self.scan.as_mut()?;
        let value = scan.pick(backward, source, members)?;
        let suffix = scan.suffix(&chars, settings);
        Some(scan.replace(&chars, &value, suffix.as_deref()))
    }
}

/// `NSCharacterSet whitespaceCharacterSet`: tab and the Unicode space
/// separators, not line breaks.
fn is_space(c: char) -> bool {
    c == '\t'
        || (c.is_whitespace()
            && !matches!(
                c,
                '\n' | '\r' | '\x0b' | '\x0c' | '\u{85}' | '\u{2028}' | '\u{2029}'
            ))
}

impl Scan {
    /// `constructCache`.
    fn new(chars: &[char], selection: Range<usize>, settings: &Settings) -> Self {
        let caret = selection.start;
        let start = chars[..caret]
            .iter()
            .rposition(|c| is_space(*c) || *c == ',')
            .map_or(0, |i| i + 1);
        let mut pattern: String = chars[start..caret].iter().collect();
        let at_start = start == 0;
        let (kind, prefix) = match pattern.chars().next() {
            Some('/') if at_start => (Kind::Command, Some('/')),
            Some('@') => (Kind::Nick, Some('@')),
            Some('#') => (Kind::Channel, None),
            _ => (Kind::Nick, None),
        };
        if prefix.is_some() {
            pattern.remove(0);
        }
        let suffix_at = if !selection.is_empty() {
            selection
        } else {
            suffix_after(chars, caret, kind, settings)
        };
        Scan {
            kind,
            pattern,
            prefix,
            pattern_at: start..caret,
            old_suffix: chars[suffix_at.clone()].iter().collect(),
            suffix_at,
            at_start,
            index: None,
            left: None,
        }
    }

    /// `performCompletion_step1`: the next match, with its prefix.
    fn pick(
        &mut self,
        backward: bool,
        source: &Source,
        members: impl FnOnce() -> Option<Vec<Member>>,
    ) -> Option<String> {
        let empty = self.pattern.is_empty();
        if empty && self.kind != Kind::Nick {
            return None;
        }
        // (shown, matched against). Textual compares `lowercaseString`s,
        // whatever the server's casemap.
        let mut choices: Vec<(String, String)> = Vec::new();
        match self.kind {
            Kind::Command => {
                let mut names = source.commands.clone();
                names.sort();
                choices.extend(names.into_iter().map(|n| (n.clone(), n.to_lowercase())));
            }
            Kind::Channel => {
                choices.extend(
                    source
                        .channels
                        .iter()
                        .map(|c| (c.clone(), c.to_lowercase())),
                );
            }
            Kind::Nick => {
                let mut list = members()?;
                let heaviest = if empty {
                    list.sort_by_key(|m| m.nick.to_lowercase());
                    let max = list.iter().map(|m| m.weight).fold(f64::MIN, f64::max);
                    let differ = list.iter().any(|m| m.weight != max);
                    list.iter().position(|m| m.weight == max).filter(|_| differ)
                } else {
                    // Stable, so equal weights keep member-list order (rank,
                    // then nick), Textual's `compareUsingRank`.
                    list.sort_by(|a, b| b.weight.total_cmp(&a.weight));
                    None
                };
                if let Some(i) = heaviest {
                    let m = list.remove(i);
                    list.insert(0, m);
                }
                for m in &list {
                    add_nick(&mut choices, &m.nick, !empty);
                }
                for name in STATIC_NAMES
                    .iter()
                    .copied()
                    .chain(source.network.as_deref())
                {
                    add_nick(&mut choices, name, false);
                }
            }
        }
        let pattern = self.pattern.to_lowercase();
        let matched: Vec<&String> = choices
            .iter()
            .filter(|(_, key)| empty || key.starts_with(&pattern))
            .map(|(shown, _)| shown)
            .collect();
        if matched.is_empty() {
            return None;
        }
        let n = matched.len();
        let index = match self.index {
            Some(i) if i < n && backward => (i + n - 1) % n,
            Some(i) if i < n => (i + 1) % n,
            _ => 0,
        };
        self.index = Some(index);
        let mut value = self.prefix.map(String::from).unwrap_or_default();
        value.push_str(matched[index]);
        Some(value)
    }

    /// `performCompletion_step3`: what follows the completed word.
    fn suffix(&self, chars: &[char], settings: &Settings) -> Option<String> {
        let in_old = self.old_suffix.chars().next_back().is_some_and(is_space);
        // Textual checks `next < length - 1`, which misses a space that is
        // the last character and doubles it; the last character counts here.
        let in_place = in_old || chars.get(self.suffix_at.end).is_some_and(|c| is_space(*c));
        if self.kind == Kind::Nick && self.at_start {
            let user = settings.suffix.as_deref().filter(|s| !s.is_empty());
            if in_place {
                match user {
                    Some(s) if s.chars().next_back().is_some_and(is_space) => {
                        let len = s.chars().count();
                        if !in_old && len > 1 {
                            Some(s.chars().take(len - 1).collect())
                        } else if in_old {
                            Some(s.to_owned())
                        } else {
                            None
                        }
                    }
                    other => other.map(str::to_owned),
                }
            } else {
                match user {
                    None => (!settings.no_whitespace).then(|| " ".to_owned()),
                    Some(s) => Some(s.to_owned()),
                }
            }
        } else {
            (!in_place).then(|| " ".to_owned())
        }
    }

    /// `performCompletion_step4`: replaces the pattern and the old suffix.
    fn replace(&mut self, chars: &[char], value: &str, suffix: Option<&str>) -> Edit {
        let start = self.pattern_at.start;
        let end = (self.suffix_at.start + self.suffix_at.len()).min(chars.len());
        let inserted = format!("{value}{}", suffix.unwrap_or_default());
        let inserted_len = inserted.chars().count();
        let text: String = chars[..start]
            .iter()
            .chain(inserted.chars().collect::<Vec<_>>().iter())
            .chain(chars[end..].iter())
            .collect();
        let caret = start + inserted_len;
        let pattern_len = self.pattern_at.len();
        self.suffix_at = start + pattern_len..start + inserted_len.max(pattern_len);
        self.left = Some((text.clone(), caret));
        Edit { text, caret }
    }
}

/// `constructCachedCompletionSuffix` without a selection.
fn suffix_after(chars: &[char], caret: usize, kind: Kind, settings: &Settings) -> Range<usize> {
    if kind == Kind::Nick
        && let Some(user) = settings.suffix.as_deref().filter(|s| !s.is_empty())
        // Textual compares the found range's length, which is the suffix's
        // own length, with 30.
        && user.chars().count() < 30
    {
        let user: Vec<char> = user.chars().collect();
        let found = (caret..=chars.len().saturating_sub(user.len()))
            .find(|&i| chars[i..].starts_with(&user));
        if let Some(at) = found
            && !chars[caret..at].iter().any(|c| is_space(*c))
        {
            return caret..at + user.len();
        }
    }
    if settings.cut_forward {
        let stop = chars[caret..]
            .iter()
            .position(|c| is_space(*c) || *c == ':' || *c == ',')
            .map_or(chars.len(), |i| caret + i);
        return caret..stop;
    }
    caret..caret
}

/// Textual's `addNickname`: the nick, and with `trimmed` also its spelling
/// without leading `^[]-_`{}\`, unless that spelling is already offered.
fn add_nick(choices: &mut Vec<(String, String)>, nick: &str, trimmed: bool) {
    choices.push((nick.to_owned(), nick.to_lowercase()));
    if !trimmed {
        return;
    }
    let short = nick.trim_start_matches(TRIMMED);
    if !short.is_empty() && short != nick {
        let key = short.to_lowercase();
        if !choices.iter().any(|(_, k)| *k == key) {
            choices.push((nick.to_owned(), key));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Source {
        Source {
            commands: ["join", "part", "msg", "me", "mode", "quit"]
                .map(String::from)
                .to_vec(),
            channels: ["#here", "#other", "#hello", "bob"]
                .map(String::from)
                .to_vec(),
            network: Some("ErgoTest".into()),
        }
    }

    fn members(list: &[(&str, f64)]) -> Vec<Member> {
        list.iter()
            .map(|(n, w)| Member {
                nick: (*n).to_owned(),
                weight: *w,
            })
            .collect()
    }

    /// Tabs at the caret marked `|`; returns each result with its caret.
    fn tabs(c: &mut Completion, text: &str, list: &[(&str, f64)], keys: &str) -> Vec<String> {
        let settings = Settings::default();
        let caret = text.find('|').expect("a caret");
        let mut text = text.replace('|', "");
        let mut at = text[..caret].chars().count();
        let mut out = Vec::new();
        for k in keys.chars() {
            let edit = c.complete(
                &text,
                at..at,
                k == 'S',
                &source(),
                || Some(members(list)),
                &settings,
            );
            match edit {
                Some(e) => {
                    text = e.text;
                    at = e.caret;
                    let byte = text.char_indices().nth(at).map_or(text.len(), |(i, _)| i);
                    out.push(format!("{}|{}", &text[..byte], &text[byte..]));
                }
                None => out.push("none".into()),
            }
        }
        out
    }

    fn one(text: &str, list: &[(&str, f64)]) -> String {
        tabs(&mut Completion::default(), text, list, "T").remove(0)
    }

    #[test]
    fn a_nick_at_the_start_gets_a_space() {
        assert_eq!(one("al|", &[("alice", 0.0), ("bob", 0.0)]), "alice |");
        assert_eq!(one("hi al|", &[("alice", 0.0)]), "hi alice |");
        assert_eq!(one("hi al| there", &[("alice", 0.0)]), "hi alice| there");
        assert_eq!(one("al| ", &[("alice", 0.0)]), "alice| ");
    }

    #[test]
    fn the_scan_stops_at_a_comma_and_matching_only_lowercases() {
        assert_eq!(one("x,AL|", &[("alice", 0.0)]), "x,alice |");
        // Textual compares `lowercaseString`s, not the server's casemap.
        assert_eq!(one("{fo|", &[("[foo]", 0.0)]), "none");
        assert_eq!(one("[fo|", &[("[foo]", 0.0)]), "[foo] |");
    }

    #[test]
    fn heavier_members_come_first_and_tab_cycles_with_wrap() {
        let list = [("ann", 1.0), ("anna", 30.0), ("anton", 4.0)];
        let got = tabs(&mut Completion::default(), "an|", &list, "TTTT");
        assert_eq!(got, ["anna |", "anton |", "ann |", "anna |"]);
        let got = tabs(&mut Completion::default(), "an|", &list, "TSS");
        assert_eq!(got, ["anna |", "ann |", "anton |"]);
    }

    #[test]
    fn equal_weights_keep_member_list_order() {
        let list = [("zed", 0.0), ("zack", 0.0)];
        assert_eq!(one("z|", &list), "zed |");
    }

    #[test]
    fn an_empty_pattern_is_alphabetical_with_only_the_heaviest_first() {
        let list = [("carol", 0.0), ("Bob", 2.0), ("alice", 5.0), ("dave", 6.0)];
        let got = tabs(&mut Completion::default(), "|", &list, "TTTTT");
        assert_eq!(got, ["dave |", "alice |", "Bob |", "carol |", "NickServ |"]);
        let flat = [("carol", 1.0), ("Bob", 1.0)];
        assert_eq!(one("|", &flat), "Bob |");
    }

    #[test]
    fn trimmed_spellings_match_and_complete_to_the_real_nick() {
        assert_eq!(one("fo|", &[("_foo", 0.0)]), "_foo |");
        assert_eq!(one("_f|", &[("_foo", 0.0)]), "_foo |");
        // Without a pattern the trimmed spelling is not added.
        let got = tabs(&mut Completion::default(), "|", &[("_foo", 0.0)], "TT");
        assert_eq!(got, ["_foo |", "NickServ |"]);
        // A trimmed spelling another nick already has is not added, so
        // "fo" finds only "foo" (Textual's `containsObject:` check).
        let got = tabs(
            &mut Completion::default(),
            "fo|",
            &[("foo", 1.0), ("_foo", 0.0)],
            "TT",
        );
        assert_eq!(got, ["foo |", "foo |"]);
    }

    #[test]
    fn services_app_and_network_names_come_last() {
        assert_eq!(one("Chan|", &[("chandler", 0.0)]), "chandler |");
        let got = tabs(
            &mut Completion::default(),
            "Chan|",
            &[("chandler", 0.0)],
            "TT",
        );
        assert_eq!(got[1], "ChanServ |");
        assert_eq!(one("Tex|", &[]), "Textual |");
        assert_eq!(one("ergo|", &[]), "ErgoTest |");
    }

    #[test]
    fn an_at_sign_is_stripped_and_put_back() {
        assert_eq!(one("hi @al|", &[("alice", 0.0)]), "hi @alice |");
    }

    #[test]
    fn a_slash_at_the_start_completes_commands() {
        assert_eq!(one("/jo|", &[]), "/join |");
        let got = tabs(&mut Completion::default(), "/m|", &[], "TTT");
        assert_eq!(got, ["/me |", "/mode |", "/msg |"]);
        // Not at the start, a slash is part of a nick pattern.
        assert_eq!(one("x /jo|", &[("/joe", 0.0)]), "x /joe |");
        assert_eq!(one("/|", &[]), "none");
    }

    #[test]
    fn a_hash_completes_channels_focused_first() {
        let got = tabs(&mut Completion::default(), "see #h|", &[], "TTT");
        assert_eq!(got, ["see #here |", "see #hello |", "see #here |"]);
    }

    #[test]
    fn the_console_offers_no_nicks() {
        let mut c = Completion::default();
        let e = c.complete("al", 2..2, false, &source(), || None, &Settings::default());
        assert_eq!(e, None);
    }

    #[test]
    fn no_match_changes_nothing() {
        assert_eq!(one("zz|", &[("alice", 0.0)]), "none");
    }

    #[test]
    fn a_change_between_tabs_starts_a_new_scan() {
        let list = [("anna", 2.0), ("anton", 1.0)];
        let mut c = Completion::default();
        let settings = Settings::default();
        let src = source();
        let e = c
            .complete("an", 2..2, false, &src, || Some(members(&list)), &settings)
            .unwrap();
        assert_eq!(e.text, "anna ");
        // The caret moved: a new scan of "anna" from its start.
        let e = c
            .complete(
                &e.text,
                4..4,
                false,
                &src,
                || Some(members(&list)),
                &settings,
            )
            .unwrap();
        assert_eq!(e.text, "anna ");
        let e = c
            .complete(
                &e.text,
                e.caret..e.caret,
                false,
                &src,
                || Some(members(&list)),
                &settings,
            )
            .unwrap();
        assert_eq!(e.text, "anna ");
        // Typed after: the text differs, so "anna x" scans "x" and fails.
        assert_eq!(
            c.complete(
                "anna x",
                6..6,
                false,
                &src,
                || Some(members(&list)),
                &settings
            ),
            None
        );
    }

    #[test]
    fn the_user_suffix_follows_a_nick_at_the_start_only() {
        let settings = Settings {
            suffix: Some(": ".into()),
            ..Settings::default()
        };
        let list = || Some(members(&[("alice", 0.0), ("alfred", 0.0)]));
        let mut c = Completion::default();
        let e = c
            .complete("al", 2..2, false, &source(), list, &settings)
            .unwrap();
        assert_eq!(e.text, "alice: ");
        // The next Tab replaces the suffix it put there.
        let e = c
            .complete(&e.text, e.caret..e.caret, false, &source(), list, &settings)
            .unwrap();
        assert_eq!(e.text, "alfred: ");
        let mut c = Completion::default();
        let e = c
            .complete("hi al", 5..5, false, &source(), list, &settings)
            .unwrap();
        assert_eq!(e.text, "hi alice ");
        // An existing suffix after the caret is replaced, not doubled.
        let mut c = Completion::default();
        let e = c
            .complete("al: x", 2..2, false, &source(), list, &settings)
            .unwrap();
        assert_eq!(e.text, "alice: x");
    }

    #[test]
    fn no_whitespace_and_cut_forward() {
        let list = || Some(members(&[("alice", 0.0)]));
        let bare = Settings {
            no_whitespace: true,
            ..Settings::default()
        };
        let e = Completion::default().complete("al", 2..2, false, &source(), list, &bare);
        assert_eq!(e.unwrap().text, "alice");
        let cut = Settings {
            cut_forward: true,
            ..Settings::default()
        };
        let e = Completion::default().complete("alxyz, hi", 2..2, false, &source(), list, &cut);
        assert_eq!(e.unwrap().text, "alice , hi");
    }

    #[test]
    fn a_selection_is_replaced() {
        let list = || Some(members(&[("alice", 0.0)]));
        let e = Completion::default().complete(
            "alXX yo",
            2..4,
            false,
            &source(),
            list,
            &Settings::default(),
        );
        assert_eq!(e.unwrap().text, "alice yo");
    }

    #[test]
    fn non_ascii_text_keeps_character_offsets() {
        assert_eq!(one("héllo al|", &[("alice", 0.0)]), "héllo alice |");
        assert_eq!(one("é|", &[("Émile", 0.0)]), "Émile |");
    }
}
