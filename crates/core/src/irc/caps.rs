//! IRCv3 capability negotiation (CAP LS 302, REQ, ACK, NAK, NEW, DEL) as a
//! step function. The same `Caps` runs during registration and afterwards,
//! when `cap-notify` lets the server add or remove capabilities.

use std::collections::{BTreeMap, BTreeSet};

use super::{Message, WireLine};

/// The capabilities phase 2 asks for when the server offers them. `sasl` is
/// added separately, only when the server's `Auth` uses it.
pub const WANTED: &[&str] = &[
    "account-notify",
    "away-notify",
    "batch",
    "cap-notify",
    "chghost",
    "echo-message",
    "extended-join",
    "invite-notify",
    "message-tags",
    "multi-prefix",
    "server-time",
    "setname",
    "userhost-in-names",
];

/// A whole line, CRLF included, as the server must accept it.
const LINE_LIMIT: usize = 512;
const REQ_OVERHEAD: usize = "CAP REQ :\r\n".len();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caps {
    wanted: BTreeSet<String>,
    /// Name to value (`sasl` to `PLAIN,EXTERNAL`; empty when none was given).
    offered: BTreeMap<String, String>,
    enabled: BTreeSet<String>,
    /// False until the last line of the LS reply arrives.
    listed: bool,
    /// Caps sent in a REQ and not yet answered. Every key is a `wanted`
    /// name, so REQ lines are never built from server text.
    outstanding: BTreeMap<String, Request>,
    next_batch: u32,
}

/// The REQ line an outstanding cap went out in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Request {
    batch: u32,
    /// Alone on its line, so a NAK refuses this cap itself.
    alone: bool,
}

/// What one CAP line changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CapStep {
    pub send: Vec<WireLine>,
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}

impl Caps {
    /// Panics on a name that is empty, holds a space or line break, starts
    /// with `-` or `:`, or cannot fit a REQ line: the names come from this
    /// crate's constants, not from input.
    pub fn new<'a>(wanted: impl IntoIterator<Item = &'a str>) -> Self {
        let wanted: BTreeSet<String> = wanted.into_iter().map(str::to_owned).collect();
        for name in &wanted {
            assert!(
                !name.is_empty()
                    && !name.starts_with(['-', ':'])
                    && !name.contains([' ', '\r', '\n', '\0'])
                    && REQ_OVERHEAD + name.len() <= LINE_LIMIT,
                "bad capability name {name:?}"
            );
        }
        Caps {
            wanted,
            offered: BTreeMap::new(),
            enabled: BTreeSet::new(),
            listed: false,
            outstanding: BTreeMap::new(),
            next_batch: 0,
        }
    }

    pub fn ls_request() -> WireLine {
        wire(&["LS", "302"])
    }

    pub fn end_request() -> WireLine {
        wire(&["END"])
    }

    pub fn is_enabled(&self, name: &str) -> bool {
        self.enabled.contains(name)
    }

    pub fn enabled(&self) -> impl Iterator<Item = &str> {
        self.enabled.iter().map(String::as_str)
    }

    /// The value the server gave with an offered capability, `Some("")` when
    /// it gave none.
    pub fn offered_value(&self, name: &str) -> Option<&str> {
        self.offered.get(name).map(String::as_str)
    }

    /// The LS reply is complete and every REQ has been answered.
    pub fn settled(&self) -> bool {
        self.listed && self.outstanding.is_empty()
    }

    /// Takes a `CAP` message from the server; other commands, and CAP lines
    /// without a list after the subcommand, are ignored.
    pub fn on_cap(&mut self, msg: &Message) -> CapStep {
        let mut step = CapStep::default();
        if msg.command != "CAP" || msg.params.len() < 2 {
            return step;
        }
        let (more, list) = match &msg.params[2..] {
            [star, list] if star == "*" => (true, list.as_str()),
            [list] if list != "*" => (false, list.as_str()),
            _ => return step,
        };
        let names = list.split_ascii_whitespace();
        match msg.params[1].to_ascii_uppercase().as_str() {
            "LS" => {
                self.offer(list);
                if !more {
                    self.listed = true;
                    self.request_missing(&mut step);
                }
            }
            "NEW" => {
                self.offer(list);
                self.request_missing(&mut step);
            }
            "DEL" => {
                for name in names {
                    self.offered.remove(name);
                    self.outstanding.remove(name);
                    if self.enabled.remove(name) {
                        step.disabled.push(name.to_owned());
                    }
                }
            }
            "ACK" => {
                let answered = self.answer(names.clone().map(|n| n.trim_start_matches('-')));
                for name in names {
                    let (off, base) = match name.strip_prefix('-') {
                        Some(base) => (true, base),
                        None => (false, name),
                    };
                    let Some((name, _)) = answered.iter().find(|(n, _)| n == base) else {
                        continue;
                    };
                    if off {
                        if self.enabled.remove(name) {
                            step.disabled.push(name.clone());
                        }
                    } else if self.enabled.insert(name.clone()) {
                        step.enabled.push(name.clone());
                    }
                }
            }
            "NAK" => {
                for (name, request) in self.answer(names) {
                    if !request.alone {
                        self.request(vec![name], &mut step);
                    }
                }
            }
            _ => {}
        }
        step
    }

    fn offer(&mut self, list: &str) {
        for item in list.split_ascii_whitespace() {
            let (name, value) = item.split_once('=').unwrap_or((item, ""));
            self.offered.insert(name.to_owned(), value.to_owned());
        }
    }

    /// Settles every REQ an ACK or NAK names any cap of, and returns all
    /// their caps. A server must answer a REQ whole, so the members it left
    /// out are answered too; names we never requested are ignored.
    fn answer<'a>(&mut self, names: impl Iterator<Item = &'a str>) -> Vec<(String, Request)> {
        let batches: BTreeSet<u32> = names
            .filter_map(|n| self.outstanding.get(n))
            .map(|r| r.batch)
            .collect();
        let (answered, rest): (BTreeMap<_, _>, _) = std::mem::take(&mut self.outstanding)
            .into_iter()
            .partition(|(_, r)| batches.contains(&r.batch));
        self.outstanding = rest;
        answered.into_iter().collect()
    }

    /// REQ lines for every wanted, offered cap not yet enabled or requested,
    /// packed into as few lines as fit.
    fn request_missing(&mut self, step: &mut CapStep) {
        let missing: Vec<String> = self
            .wanted
            .iter()
            .filter(|n| self.offered.contains_key(*n))
            .filter(|n| !self.enabled.contains(*n) && !self.outstanding.contains_key(*n))
            .cloned()
            .collect();
        for batch in pack(missing) {
            self.request(batch, step);
        }
    }

    fn request(&mut self, batch: Vec<String>, step: &mut CapStep) {
        step.send.push(wire(&["REQ", &batch.join(" ")]));
        let request = Request {
            batch: self.next_batch,
            alone: batch.len() == 1,
        };
        self.next_batch = self.next_batch.wrapping_add(1);
        self.outstanding
            .extend(batch.into_iter().map(|name| (name, request)));
    }
}

/// Greedy packing into `CAP REQ :a b c` lines of at most 512 bytes.
fn pack(names: Vec<String>) -> Vec<Vec<String>> {
    let mut batches: Vec<Vec<String>> = Vec::new();
    let mut used = 0;
    for name in names {
        match batches.last_mut() {
            Some(batch) if used + 1 + name.len() <= LINE_LIMIT => {
                used += 1 + name.len();
                batch.push(name);
            }
            _ => {
                used = REQ_OVERHEAD + name.len();
                batches.push(vec![name]);
            }
        }
    }
    batches
}

/// Only for parameters built from constants and `wanted` names, which
/// `Caps::new` checked, so the line is always valid.
fn wire(params: &[&str]) -> WireLine {
    Message::new("CAP", params)
        .to_wire()
        .expect("CAP lines are built from validated capability names")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(raw: &str) -> Message {
        Message::parse(raw).unwrap()
    }

    fn sent(step: &CapStep) -> Vec<&str> {
        step.send
            .iter()
            .map(|l| l.as_str().trim_end_matches("\r\n"))
            .collect()
    }

    #[test]
    fn packs_requests_under_the_line_limit() {
        let names: Vec<String> = (0..60)
            .map(|i| format!("vendor.example/cap-{i:02}"))
            .collect();
        let mut caps = Caps::new(names.iter().map(String::as_str));
        let step = caps.on_cap(&line(&format!(":s CAP * LS :{}", names.join(" "))));
        assert!(step.send.len() > 1, "60 long names cannot fit in one line");
        assert!(step.send.iter().all(|l| l.len() <= 512));
        let mut requested: Vec<&str> = sent(&step)
            .iter()
            .flat_map(|l| l.strip_prefix("CAP REQ :").unwrap().split(' '))
            .collect();
        requested.sort_unstable();
        let mut expected: Vec<&str> = names.iter().map(String::as_str).collect();
        expected.sort_unstable();
        assert_eq!(requested, expected);
        // Greedy: every line but the last is too full to take the next name.
        for l in &step.send[..step.send.len() - 1] {
            assert!(l.len() + " vendor.example/cap-00".len() > 512);
        }
    }

    #[test]
    fn requests_only_offered_wanted_caps() {
        let mut caps = Caps::new(["multi-prefix", "batch"]);
        let step = caps.on_cap(&line(":s CAP * LS :multi-prefix sasl=PLAIN"));
        assert_eq!(sent(&step), ["CAP REQ multi-prefix"]);
        assert_eq!(caps.offered_value("sasl"), Some("PLAIN"));
        assert!(!caps.settled());
        caps.on_cap(&line(":s CAP * ACK multi-prefix"));
        assert!(caps.settled());
        assert!(caps.is_enabled("multi-prefix"));
    }

    #[test]
    fn an_unmatched_nak_requests_nothing_from_server_text() {
        let mut caps = Caps::new(WANTED.iter().copied());
        caps.on_cap(&line(":s CAP * LS :a"));
        let step = caps.on_cap(&line(":s CAP * NAK :a\0b a\0b2"));
        assert!(step.send.is_empty(), "{:?}", step.send);
        let long = "x".repeat(600);
        let step = caps.on_cap(&line(&format!(":s CAP * NAK :{long} {long}y")));
        assert!(step.send.is_empty(), "{:?}", step.send);
        assert!(caps.settled());
    }

    #[test]
    fn an_unmatched_nak_leaves_the_real_request_outstanding() {
        let mut caps = Caps::new(["away-notify", "multi-prefix"]);
        caps.on_cap(&line(":s CAP * LS :away-notify multi-prefix"));
        let step = caps.on_cap(&line(":s CAP * NAK :foo bar"));
        assert!(step.send.is_empty(), "{:?}", sent(&step));
        assert!(!caps.settled());
        caps.on_cap(&line(":s CAP * ACK :away-notify multi-prefix"));
        assert!(caps.settled());
    }

    fn requested_ab() -> Caps {
        let mut caps = Caps::new(["away-notify", "multi-prefix"]);
        let step = caps.on_cap(&line(":s CAP * LS :away-notify multi-prefix"));
        assert_eq!(sent(&step), ["CAP REQ :away-notify multi-prefix"]);
        caps
    }

    #[test]
    fn a_partial_ack_settles() {
        let mut caps = requested_ab();
        let step = caps.on_cap(&line(":s CAP * ACK :away-notify"));
        assert_eq!(step.enabled, ["away-notify"]);
        assert!(caps.settled());
        assert!(!caps.is_enabled("multi-prefix"));
    }

    #[test]
    fn a_superset_ack_settles_and_enables_only_what_was_requested() {
        let mut caps = requested_ab();
        let step = caps.on_cap(&line(":s CAP * ACK :away-notify multi-prefix sasl"));
        assert_eq!(step.enabled, ["away-notify", "multi-prefix"]);
        assert!(caps.settled());
        assert!(!caps.is_enabled("sasl"));
    }

    #[test]
    fn a_nak_of_a_subset_re_requests_the_batch_one_by_one() {
        let mut caps = requested_ab();
        let step = caps.on_cap(&line(":s CAP * NAK :away-notify"));
        assert_eq!(sent(&step), ["CAP REQ away-notify", "CAP REQ multi-prefix"]);
        caps.on_cap(&line(":s CAP * NAK away-notify"));
        caps.on_cap(&line(":s CAP * ACK multi-prefix"));
        assert!(caps.settled());
        assert_eq!(caps.enabled().collect::<Vec<_>>(), ["multi-prefix"]);
    }

    #[test]
    fn a_single_nak_named_twice_is_not_retried_forever() {
        let mut caps = requested_ab();
        caps.on_cap(&line(":s CAP * NAK :away-notify multi-prefix"));
        let step = caps.on_cap(&line(":s CAP * NAK :away-notify multi-prefix"));
        assert!(step.send.is_empty(), "{:?}", sent(&step));
        assert!(caps.settled());
    }

    #[test]
    fn del_of_a_pending_cap_settles() {
        let mut caps = Caps::new(["away-notify"]);
        caps.on_cap(&line(":s CAP * LS :away-notify"));
        caps.on_cap(&line(":s CAP * DEL :away-notify"));
        assert!(caps.settled());
        let step = caps.on_cap(&line(":s CAP * ACK :away-notify"));
        assert!(step.enabled.is_empty());
        assert!(!caps.is_enabled("away-notify"));
    }

    #[test]
    fn a_missing_list_is_ignored() {
        let mut caps = Caps::new(["LS", "ACK"]);
        let step = caps.on_cap(&line(":s CAP * LS"));
        assert!(step.send.is_empty(), "{:?}", sent(&step));
        assert_eq!(caps.offered_value("LS"), None);
        caps.on_cap(&line(":s CAP * LS *"));
        assert_eq!(caps.offered_value("*"), None);
        assert!(!caps.settled());
        let step = caps.on_cap(&line(":s CAP * ACK"));
        assert!(step.enabled.is_empty());
        assert!(!caps.is_enabled("ACK"));
    }

    #[test]
    fn new_and_del_after_registration() {
        let mut caps = Caps::new(["away-notify", "multi-prefix"]);
        caps.on_cap(&line(":s CAP * LS :multi-prefix"));
        caps.on_cap(&line(":s CAP me ACK :multi-prefix"));
        let step = caps.on_cap(&line(":s CAP me NEW :away-notify foo"));
        assert_eq!(sent(&step), ["CAP REQ away-notify"]);
        let step = caps.on_cap(&line(":s CAP me ACK :away-notify"));
        assert_eq!(step.enabled, ["away-notify"]);
        let step = caps.on_cap(&line(":s CAP me DEL :away-notify foo"));
        assert_eq!(step.disabled, ["away-notify"]);
        assert!(!caps.is_enabled("away-notify"));
        assert_eq!(caps.enabled().collect::<Vec<_>>(), ["multi-prefix"]);
    }
}
