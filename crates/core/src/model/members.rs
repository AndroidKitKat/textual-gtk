//! The members of one joined channel, kept in display order: highest
//! PREFIX rank first, then by folded nick. Every change returns the
//! positional `MemberDiff` that brings a list in the old order to the new
//! one, so the view never sorts.

use std::collections::HashMap;

use crate::irc::casemap::{CaseMapping, NickKey};

/// A member's prefix modes as bits over the ISUPPORT PREFIX index, bit 0
/// being the highest rank. Unlike Textual's mode string, there is no order
/// to keep, and no prefix letter is hard-coded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModeBits(u16);

impl ModeBits {
    /// PREFIX ranks past this many are ignored; real servers have six.
    pub const RANKS: usize = 16;

    pub fn with(self, rank: usize) -> Self {
        if rank < Self::RANKS {
            ModeBits(self.0 | 1 << rank)
        } else {
            self
        }
    }

    pub fn without(self, rank: usize) -> Self {
        if rank < Self::RANKS {
            ModeBits(self.0 & !(1 << rank))
        } else {
            self
        }
    }

    /// The highest rank held, as a PREFIX index.
    pub fn highest(self) -> Option<usize> {
        (self.0 != 0).then(|| self.0.trailing_zeros() as usize)
    }

    pub fn ranks(self) -> impl Iterator<Item = usize> {
        (0..Self::RANKS).filter(move |r| self.0 & (1 << r) != 0)
    }
}

/// What we know about one member. There is no network-wide user table;
/// AWAY, CHGHOST and ACCOUNT update every channel's copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub nick: String,
    pub modes: ModeBits,
    pub away: bool,
    pub user: Option<String>,
    pub host: Option<String>,
    pub account: Option<String>,
}

impl Member {
    pub fn new(nick: &str) -> Self {
        Member {
            nick: nick.to_owned(),
            modes: ModeBits::default(),
            away: false,
            user: None,
            host: None,
            account: None,
        }
    }
}

/// What one row of the member list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRow {
    pub nick: String,
    /// The highest prefix mode and its symbol, such as `('o', '@')`.
    pub prefix: Option<(char, char)>,
    pub away: bool,
}

/// One edit to a list in display order. Applied in sequence, the diffs a
/// change returns turn the old list into the new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberDiff {
    /// The whole list changed: read it again.
    Reset,
    Insert {
        at: usize,
        row: MemberRow,
    },
    Remove {
        at: usize,
    },
    Update {
        at: usize,
        row: MemberRow,
    },
    /// Remove the row at `from`, then insert `row` at `to`, an index into
    /// the list without it.
    Move {
        from: usize,
        to: usize,
        row: MemberRow,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Slot {
    /// `ModeBits::highest`, with no rank sorting last.
    rank: usize,
    key: NickKey,
}

impl Slot {
    fn of(key: &NickKey, member: &Member) -> Self {
        Slot {
            rank: member.modes.highest().unwrap_or(usize::MAX),
            key: key.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Members {
    by_key: HashMap<NickKey, Member>,
    /// Always sorted. Positions in `MemberDiff` index this.
    order: Vec<Slot>,
}

impl Members {
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn get(&self, key: &NickKey) -> Option<&Member> {
        self.by_key.get(key)
    }

    pub fn contains(&self, key: &NickKey) -> bool {
        self.by_key.contains_key(key)
    }

    /// Members in display order.
    pub fn iter(&self) -> impl Iterator<Item = &Member> {
        self.order.iter().map(|slot| &self.by_key[&slot.key])
    }

    /// Rows in display order. `prefix` is `Isupport::prefix`.
    pub fn rows<'a>(&'a self, prefix: &'a [(char, char)]) -> impl Iterator<Item = MemberRow> + 'a {
        self.iter().map(move |m| row(m, prefix))
    }

    /// Adds a member; `None` if one with this key is already here.
    pub(crate) fn insert(
        &mut self,
        key: NickKey,
        member: Member,
        prefix: &[(char, char)],
    ) -> Option<MemberDiff> {
        if self.by_key.contains_key(&key) {
            return None;
        }
        let slot = Slot::of(&key, &member);
        let at = self.order.binary_search(&slot).unwrap_err();
        let row = row(&member, prefix);
        self.order.insert(at, slot);
        self.by_key.insert(key, member);
        Some(MemberDiff::Insert { at, row })
    }

    pub(crate) fn remove(&mut self, key: &NickKey) -> Option<MemberDiff> {
        let member = self.by_key.remove(key)?;
        let at = self.position(&Slot::of(key, &member));
        self.order.remove(at);
        Some(MemberDiff::Remove { at })
    }

    /// Edits a member in place; `None` if it is not here or nothing changed.
    pub(crate) fn update(
        &mut self,
        key: &NickKey,
        prefix: &[(char, char)],
        edit: impl FnOnce(&mut Member),
    ) -> Option<MemberDiff> {
        let mut member = self.by_key.get(key)?.clone();
        edit(&mut member);
        self.replace(key, key.clone(), member, prefix)
    }

    /// NICK: re-keys a member and takes the new spelling. If another member
    /// already has the new key, the old entry is dropped.
    pub(crate) fn rename(
        &mut self,
        old: &NickKey,
        new: NickKey,
        nick: &str,
        prefix: &[(char, char)],
    ) -> Option<MemberDiff> {
        if *old != new && self.by_key.contains_key(&new) {
            return self.remove(old);
        }
        let mut member = self.by_key.get(old)?.clone();
        nick.clone_into(&mut member.nick);
        self.replace(old, new, member, prefix)
    }

    /// Swaps in a complete list (end of NAMES). A key given twice keeps its
    /// last entry.
    pub(crate) fn replace_all(&mut self, entries: Vec<(NickKey, Member)>) -> MemberDiff {
        self.by_key = entries.into_iter().collect();
        self.resort();
        MemberDiff::Reset
    }

    /// Re-keys every member under a new casemap.
    pub(crate) fn rekey(&mut self, casemap: CaseMapping) -> MemberDiff {
        let members = std::mem::take(&mut self.by_key).into_values();
        self.by_key = members.map(|m| (casemap.nick(&m.nick), m)).collect();
        self.resort();
        MemberDiff::Reset
    }

    fn resort(&mut self) {
        self.order = self.by_key.iter().map(|(k, m)| Slot::of(k, m)).collect();
        self.order.sort();
    }

    fn position(&self, slot: &Slot) -> usize {
        self.order
            .binary_search(slot)
            .expect("every member has a slot")
    }

    fn replace(
        &mut self,
        old: &NickKey,
        new: NickKey,
        member: Member,
        prefix: &[(char, char)],
    ) -> Option<MemberDiff> {
        let before = &self.by_key[old];
        if *old == new && *before == member {
            return None;
        }
        let from = self.position(&Slot::of(old, before));
        self.order.remove(from);
        self.by_key.remove(old);
        let slot = Slot::of(&new, &member);
        let to = self.order.binary_search(&slot).unwrap_err();
        let row = row(&member, prefix);
        self.order.insert(to, slot);
        self.by_key.insert(new, member);
        Some(if from == to {
            MemberDiff::Update { at: to, row }
        } else {
            MemberDiff::Move { from, to, row }
        })
    }
}

fn row(member: &Member, prefix: &[(char, char)]) -> MemberRow {
    MemberRow {
        nick: member.nick.clone(),
        prefix: member.modes.highest().and_then(|r| prefix.get(r).copied()),
        away: member.away,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &[(char, char)] = &[('q', '~'), ('o', '@'), ('v', '+')];
    const RFC: CaseMapping = CaseMapping::Rfc1459;

    fn member(nick: &str, ranks: &[usize]) -> (NickKey, Member) {
        let mut m = Member::new(nick);
        for r in ranks {
            m.modes = m.modes.with(*r);
        }
        (RFC.nick(nick), m)
    }

    fn shown(members: &Members) -> Vec<String> {
        members
            .rows(PREFIX)
            .map(|r| {
                format!(
                    "{}{}",
                    r.prefix.map_or(String::new(), |p| p.1.to_string()),
                    r.nick
                )
            })
            .collect()
    }

    /// Applies diffs to a plain list the way the view does, to check that
    /// positions are right.
    fn apply(view: &mut Vec<String>, diff: &MemberDiff, members: &Members) {
        let text = |r: &MemberRow| {
            format!(
                "{}{}",
                r.prefix.map_or(String::new(), |p| p.1.to_string()),
                r.nick
            )
        };
        match diff {
            MemberDiff::Reset => *view = shown(members),
            MemberDiff::Insert { at, row } => view.insert(*at, text(row)),
            MemberDiff::Remove { at } => {
                view.remove(*at);
            }
            MemberDiff::Update { at, row } => view[*at] = text(row),
            MemberDiff::Move { from, to, row } => {
                view.remove(*from);
                view.insert(*to, text(row));
            }
        }
    }

    #[test]
    fn mode_bits_rank_by_prefix_index() {
        let bits = ModeBits::default().with(2).with(1);
        assert_eq!(bits.highest(), Some(1));
        assert_eq!(bits.without(1).highest(), Some(2));
        assert_eq!(bits.ranks().collect::<Vec<_>>(), [1, 2]);
        assert_eq!(ModeBits::default().with(16), ModeBits::default());
        assert_eq!(ModeBits::default().highest(), None);
    }

    #[test]
    fn order_is_rank_then_folded_nick() {
        let mut m = Members::default();
        m.replace_all(vec![
            member("carol", &[]),
            member("Bob", &[2]),
            member("alice", &[1]),
            member("dave", &[1, 2]),
            member("zed", &[0]),
            member("[x]", &[]),
        ]);
        // `[` folds to `{`, which sorts after letters.
        assert_eq!(
            shown(&m),
            ["~zed", "@alice", "@dave", "+Bob", "carol", "[x]"]
        );
    }

    #[test]
    fn every_diff_keeps_a_view_in_step() {
        let mut m = Members::default();
        let mut view = Vec::new();
        let step = |m: &mut Members, d: Option<MemberDiff>, view: &mut Vec<String>| {
            let d = d.expect("a change");
            apply(view, &d, m);
            assert_eq!(*view, shown(m), "after {d:?}");
        };
        for (k, v) in [
            member("bob", &[]),
            member("alice", &[]),
            member("carol", &[2]),
        ] {
            let d = m.insert(k, v, PREFIX);
            step(&mut m, d, &mut view);
        }
        let d = m.update(&RFC.nick("carol"), PREFIX, |c| c.modes = c.modes.with(1));
        assert!(matches!(d, Some(MemberDiff::Update { at: 0, .. })), "{d:?}");
        step(&mut m, d, &mut view);
        let d = m.update(&RFC.nick("bob"), PREFIX, |c| c.modes = c.modes.with(1));
        assert!(
            matches!(d, Some(MemberDiff::Move { from: 2, to: 0, .. })),
            "{d:?}"
        );
        step(&mut m, d, &mut view);
        let d = m.update(&RFC.nick("carol"), PREFIX, |c| {
            c.modes = ModeBits::default()
        });
        step(&mut m, d, &mut view);
        let d = m.rename(&RFC.nick("alice"), RFC.nick("aaron"), "aaron", PREFIX);
        assert!(matches!(d, Some(MemberDiff::Update { .. })), "{d:?}");
        step(&mut m, d, &mut view);
        let d = m.rename(&RFC.nick("aaron"), RFC.nick("zoe"), "zoe", PREFIX);
        step(&mut m, d, &mut view);
        let d = m.update(&RFC.nick("zoe"), PREFIX, |c| c.away = true);
        step(&mut m, d, &mut view);
        let d = m.remove(&RFC.nick("bob"));
        step(&mut m, d, &mut view);
        assert_eq!(view, ["carol", "zoe"]);
    }

    #[test]
    fn no_op_changes_return_nothing() {
        let mut m = Members::default();
        let (k, v) = member("bob", &[]);
        assert!(m.insert(k.clone(), v.clone(), PREFIX).is_some());
        assert_eq!(m.insert(k.clone(), v, PREFIX), None, "a duplicate insert");
        assert_eq!(m.update(&k, PREFIX, |_| {}), None);
        assert_eq!(m.remove(&RFC.nick("nobody")), None);
        assert_eq!(m.update(&RFC.nick("nobody"), PREFIX, |_| {}), None);
        assert_eq!(
            m.rename(&RFC.nick("nobody"), RFC.nick("x"), "x", PREFIX),
            None
        );
    }

    #[test]
    fn rfc1459_brackets_are_one_member_and_a_case_change_renames() {
        let mut m = Members::default();
        m.replace_all(vec![member("nick[]", &[]), member("nick{}", &[2])]);
        assert_eq!(m.len(), 1, "the later NAMES entry wins");
        let d = m.rename(&RFC.nick("nick{}"), RFC.nick("NICK[]"), "NICK[]", PREFIX);
        assert!(matches!(d, Some(MemberDiff::Update { at: 0, .. })), "{d:?}");
        assert_eq!(shown(&m), ["+NICK[]"]);
    }

    #[test]
    fn renaming_onto_another_member_drops_the_old_entry() {
        let mut m = Members::default();
        m.replace_all(vec![member("a", &[]), member("b", &[])]);
        let d = m.rename(&RFC.nick("a"), RFC.nick("b"), "b", PREFIX);
        assert_eq!(d, Some(MemberDiff::Remove { at: 0 }));
        assert_eq!(shown(&m), ["b"]);
    }

    #[test]
    fn rekey_folds_under_the_new_casemap() {
        let mut m = Members::default();
        m.replace_all(vec![member("x[", &[])]);
        assert!(m.contains(&RFC.nick("X{")));
        m.rekey(CaseMapping::Ascii);
        assert!(m.contains(&CaseMapping::Ascii.nick("X[")));
        assert!(!m.contains(&CaseMapping::Ascii.nick("x{")));
    }
}
