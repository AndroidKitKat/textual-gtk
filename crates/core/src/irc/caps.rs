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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caps {
    wanted: BTreeSet<String>,
    /// Name to value (`sasl` to `PLAIN,EXTERNAL`; empty when none was given).
    offered: BTreeMap<String, String>,
    enabled: BTreeSet<String>,
    /// False until the last line of the LS reply arrives.
    listed: bool,
    /// REQ batches sent and not yet answered, oldest first. The server
    /// answers each whole, with one ACK or one NAK.
    awaiting: Vec<BTreeSet<String>>,
}

/// What one CAP line changed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CapStep {
    pub send: Vec<WireLine>,
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}

impl Caps {
    /// Panics on a name that is empty or holds a space or line break: the
    /// names come from this crate's constants, not from input.
    pub fn new<'a>(wanted: impl IntoIterator<Item = &'a str>) -> Self {
        let wanted: BTreeSet<String> = wanted.into_iter().map(str::to_owned).collect();
        for name in &wanted {
            assert!(
                !name.is_empty() && !name.contains([' ', '\r', '\n', '\0']),
                "bad capability name {name:?}"
            );
        }
        Caps {
            wanted,
            offered: BTreeMap::new(),
            enabled: BTreeSet::new(),
            listed: false,
            awaiting: Vec::new(),
        }
    }

    pub fn ls_request() -> WireLine {
        wire(&Message::new("CAP", &["LS", "302"]))
    }

    pub fn end_request() -> WireLine {
        wire(&Message::new("CAP", &["END"]))
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
        self.listed && self.awaiting.is_empty()
    }

    /// Takes a `CAP` message from the server; other commands are ignored.
    pub fn on_cap(&mut self, msg: &Message) -> CapStep {
        let mut step = CapStep::default();
        if msg.command != "CAP" {
            return step;
        }
        let sub = msg.param(1).unwrap_or("").to_ascii_uppercase();
        let list = msg.params.last().map(String::as_str).unwrap_or("");
        match sub.as_str() {
            "LS" => {
                let more = msg.params.len() >= 4 && msg.param(2) == Some("*");
                self.offer(list);
                if !more {
                    self.listed = true;
                    step.send = self.request_missing();
                }
            }
            "NEW" => {
                self.offer(list);
                step.send = self.request_missing();
            }
            "DEL" => {
                for name in list.split_ascii_whitespace() {
                    self.offered.remove(name);
                    if self.enabled.remove(name) {
                        step.disabled.push(name.to_owned());
                    }
                }
            }
            "ACK" => {
                let names = answered(list);
                self.take_batch(&names);
                for name in names {
                    if let Some(off) = name.strip_prefix('-') {
                        if self.enabled.remove(off) {
                            step.disabled.push(off.to_owned());
                        }
                    } else if self.enabled.insert(name.to_owned()) {
                        step.enabled.push(name.to_owned());
                    }
                }
            }
            "NAK" => {
                let names = answered(list);
                let batch = self
                    .take_batch(&names)
                    .unwrap_or_else(|| names.iter().map(|n| (*n).to_owned()).collect());
                if batch.len() > 1 {
                    for name in batch {
                        let single = BTreeSet::from([name]);
                        step.send.push(req_line(&single));
                        self.awaiting.push(single);
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

    /// Removes the outstanding batch an ACK or NAK answers. Servers answer in
    /// order, but matching by content survives one that does not.
    fn take_batch(&mut self, names: &[&str]) -> Option<BTreeSet<String>> {
        let answered: BTreeSet<&str> = names.iter().map(|n| n.trim_start_matches('-')).collect();
        let index = self
            .awaiting
            .iter()
            .position(|b| b.iter().map(String::as_str).eq(answered.iter().copied()))?;
        Some(self.awaiting.remove(index))
    }

    /// REQ lines for every wanted, offered cap not yet enabled or requested,
    /// packed into as few lines as fit.
    fn request_missing(&mut self) -> Vec<WireLine> {
        let pending: BTreeSet<&String> = self.awaiting.iter().flatten().collect();
        let missing: Vec<String> = self
            .wanted
            .iter()
            .filter(|n| self.offered.contains_key(*n))
            .filter(|n| !self.enabled.contains(*n) && !pending.contains(n))
            .cloned()
            .collect();
        let batches = pack(missing);
        let lines = batches.iter().map(req_line).collect();
        self.awaiting.extend(batches);
        lines
    }
}

fn answered(list: &str) -> Vec<&str> {
    list.split_ascii_whitespace().collect()
}

/// Greedy packing into `CAP REQ :a b c` lines of at most 512 bytes.
fn pack(names: Vec<String>) -> Vec<BTreeSet<String>> {
    let overhead = "CAP REQ :\r\n".len();
    let mut batches: Vec<BTreeSet<String>> = Vec::new();
    let mut used = 0;
    for name in names {
        match batches.last_mut() {
            Some(batch) if used + 1 + name.len() <= LINE_LIMIT => {
                used += 1 + name.len();
                batch.insert(name);
            }
            _ => {
                used = overhead + name.len();
                batches.push(BTreeSet::from([name]));
            }
        }
    }
    batches
}

fn req_line(batch: &BTreeSet<String>) -> WireLine {
    let names: Vec<&str> = batch.iter().map(String::as_str).collect();
    wire(&Message::new("CAP", &["REQ", &names.join(" ")]))
}

fn wire(msg: &Message) -> WireLine {
    msg.to_wire()
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
