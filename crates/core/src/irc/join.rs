//! `JOIN` lines for a set of channels.

use super::{Message, WireLine};

/// A whole line, CRLF included, as the server must accept it.
const LINE_LIMIT: usize = 512;
const JOIN_OVERHEAD: usize = "JOIN \r\n".len();

/// Unkeyed channels first, then keyed ones, each as `JOIN #a,#b` or
/// `JOIN #c,#d key1,key2`, packed greedily under the line limit. An entry
/// that could not be told apart in a comma list (empty, or holding a space,
/// comma or line break) is dropped rather than poisoning the whole line.
pub fn join_lines(channels: &[(&str, Option<&str>)]) -> Vec<WireLine> {
    let usable = |s: &str| !s.is_empty() && !s.contains([' ', ',', '\r', '\n', '\0']);
    let (keyed, plain): (Vec<_>, Vec<_>) = channels
        .iter()
        .filter(|(name, key)| usable(name) && key.is_none_or(usable))
        .partition(|(_, key)| key.is_some());
    let mut lines = Vec::new();
    for group in [plain, keyed] {
        let (mut names, mut keys) = (String::new(), String::new());
        let mut used = JOIN_OVERHEAD;
        for (name, key) in group {
            // The comma in the name list, and for a key either the comma in
            // the key list or the space that starts it.
            let cost =
                |first: bool| name.len() + key.map_or(0, |k| k.len() + 1) + usize::from(!first);
            if used + cost(names.is_empty()) > LINE_LIMIT && !names.is_empty() {
                lines.extend(wire(&names, &keys));
                (names, keys) = (String::new(), String::new());
                used = JOIN_OVERHEAD;
            }
            used += cost(names.is_empty());
            if !names.is_empty() {
                names.push(',');
            }
            names.push_str(name);
            if let Some(key) = key {
                if !keys.is_empty() {
                    keys.push(',');
                }
                keys.push_str(key);
            }
        }
        if !names.is_empty() {
            lines.extend(wire(&names, &keys));
        }
    }
    lines
}

fn wire(names: &str, keys: &str) -> Option<WireLine> {
    let params: &[&str] = if keys.is_empty() {
        &[names]
    } else {
        &[names, keys]
    };
    Message::new("JOIN", params).to_wire().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[WireLine]) -> Vec<&str> {
        lines
            .iter()
            .map(|l| l.as_str().trim_end_matches("\r\n"))
            .collect()
    }

    #[test]
    fn keyed_and_unkeyed_go_on_separate_lines() {
        let lines = join_lines(&[
            ("#a", None),
            ("#c", Some("key1")),
            ("#b", None),
            ("#d", Some("key2")),
        ]);
        assert_eq!(text(&lines), ["JOIN #a,#b", "JOIN #c,#d key1,key2"]);
        assert!(join_lines(&[]).is_empty());
    }

    #[test]
    fn packs_under_the_line_limit_without_losing_or_reordering_channels() {
        let names: Vec<String> = (0..120).map(|i| format!("#channel-{i:03}")).collect();
        let keys: Vec<String> = (0..120).map(|i| format!("key-{i:03}")).collect();
        for keyed in [false, true] {
            let channels: Vec<(&str, Option<&str>)> = (0..120)
                .map(|i| (names[i].as_str(), keyed.then(|| keys[i].as_str())))
                .collect();
            let lines = join_lines(&channels);
            assert!(lines.len() > 1, "120 channels cannot fit in one line");
            assert!(lines.iter().all(|l| l.as_str().len() <= 512));
            let sent: Vec<&str> = text(&lines)
                .into_iter()
                .flat_map(|l| {
                    l.strip_prefix("JOIN ")
                        .unwrap()
                        .split(' ')
                        .next()
                        .unwrap()
                        .split(',')
                })
                .collect();
            let want: Vec<&str> = names.iter().map(String::as_str).collect();
            assert_eq!(sent, want);
            // Greedy: every line but the last is too full for the next entry.
            for l in &lines[..lines.len() - 1] {
                assert!(l.as_str().len() + ",#channel-000,key-000".len() > 512);
            }
        }
    }

    #[test]
    fn a_line_exactly_at_the_limit_is_kept() {
        // "JOIN " + "#" + a + " " + k + CRLF = 512.
        let name = format!("#{}", "a".repeat(250));
        let key = "k".repeat(512 - "JOIN  \r\n".len() - name.len());
        let lines = join_lines(&[(&name, Some(&key))]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].as_str().len(), 512);
    }

    #[test]
    fn an_entry_that_would_break_the_list_is_dropped() {
        let lines = join_lines(&[
            ("#ok", None),
            ("#a,#b", None),
            ("#sp ace", None),
            ("", None),
            ("#key", Some("a,b")),
            ("#key2", Some("")),
        ]);
        assert_eq!(text(&lines), ["JOIN #ok"]);
    }
}
