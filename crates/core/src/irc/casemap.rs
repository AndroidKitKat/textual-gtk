//! Nick and channel identity under the server's CASEMAPPING.
//!
//! `NickKey` and `ChanKey` can only be made by a `CaseMapping`, so a map
//! keyed by either cannot be probed with a raw string, and a nick key cannot
//! be used where a channel key is expected.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaseMapping {
    /// Only `A-Z` fold.
    Ascii,
    /// `A-Z`, and `[]\~` fold to `{}|^`. The default before 005 says
    /// otherwise, as RFC 1459 and most servers specify.
    #[default]
    Rfc1459,
    /// As rfc1459 without `~` and `^`.
    Rfc1459Strict,
}

impl CaseMapping {
    /// The mapping a 005 `CASEMAPPING=` value names, or `None` for one we do
    /// not know (the caller keeps the current mapping).
    pub fn from_token(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "ascii" => Some(CaseMapping::Ascii),
            "rfc1459" => Some(CaseMapping::Rfc1459),
            "rfc1459-strict" | "strict-rfc1459" => Some(CaseMapping::Rfc1459Strict),
            _ => None,
        }
    }

    fn fold_char(self, c: char) -> char {
        match (self, c) {
            (_, 'A'..='Z') => c.to_ascii_lowercase(),
            (CaseMapping::Ascii, _) => c,
            (_, '[') => '{',
            (_, ']') => '}',
            (_, '\\') => '|',
            (CaseMapping::Rfc1459, '~') => '^',
            _ => c,
        }
    }

    fn fold(self, name: &str) -> String {
        name.chars().map(|c| self.fold_char(c)).collect()
    }

    pub fn nick(self, name: &str) -> NickKey {
        NickKey(self.fold(name))
    }

    pub fn chan(self, name: &str) -> ChanKey {
        ChanKey(self.fold(name))
    }

    /// Whether two nicks name the same user.
    pub fn same_nick(self, a: &str, b: &str) -> bool {
        self.nick(a) == self.nick(b)
    }
}

/// A folded nick. Ordered by the folded form, which is the member-list order
/// within one rank.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NickKey(String);

/// A folded channel name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChanKey(String);

impl NickKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ChanKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NickKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for ChanKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc1459_folds_brackets_and_tilde() {
        let m = CaseMapping::Rfc1459;
        assert_eq!(m.nick("Nick[]\\~"), m.nick("nick{}|^"));
        assert!(m.same_nick("FOO", "foo"));
    }

    #[test]
    fn strict_keeps_tilde_and_caret_apart() {
        let m = CaseMapping::Rfc1459Strict;
        assert_eq!(m.nick("a[]\\"), m.nick("A{}|"));
        assert_ne!(m.nick("a~"), m.nick("a^"));
    }

    #[test]
    fn ascii_folds_letters_only() {
        let m = CaseMapping::Ascii;
        assert_eq!(m.chan("#Rust"), m.chan("#rust"));
        assert_ne!(m.chan("#a[b]"), m.chan("#a{b}"));
    }

    #[test]
    fn non_ascii_is_left_alone() {
        let m = CaseMapping::Rfc1459;
        assert_ne!(m.nick("Ärger"), m.nick("ärger"));
    }

    #[test]
    fn reads_the_005_token() {
        assert_eq!(CaseMapping::from_token("ascii"), Some(CaseMapping::Ascii));
        assert_eq!(
            CaseMapping::from_token("RFC1459"),
            Some(CaseMapping::Rfc1459)
        );
        assert_eq!(
            CaseMapping::from_token("rfc1459-strict"),
            Some(CaseMapping::Rfc1459Strict)
        );
        assert_eq!(CaseMapping::from_token("rfc7613"), None);
    }
}
