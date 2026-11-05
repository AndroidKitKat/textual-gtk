//! Feeds generated server lines through `Registration`: whatever the
//! server says, nothing panics and every line sent stays a valid IRC line.

mod support;

use support::setup;
use textual_core::irc::Message;
use textual_core::irc::caps::WANTED;
use textual_core::model::registration::{Auth, Output, Progress, Registration};
use textual_core::secret::Secret;

/// xorshift64, so every run feeds the same lines.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[(self.next() % items.len() as u64) as usize]
    }
}

fn name(rng: &mut Rng) -> String {
    let long = "l".repeat(600);
    let plain = [
        "sasl",
        "sasl=PLAIN",
        "sasl=EXTERNAL,PLAIN",
        "multi-prefix",
        "away-notify",
        "batch",
        "-batch",
        "*",
        "LS",
        "a\0b",
        "a\rb",
        "caf\u{e9}",
        "\u{1f600}",
        "=",
        "-",
    ];
    match rng.next() % 8 {
        0 => format!("{long}{}", rng.next() % 10),
        1 => "\u{e9}".repeat(300),
        _ => rng.pick(&plain).to_owned(),
    }
}

/// `ours` is the nick the ladder last sent: a refusal names it or another.
fn server_line(rng: &mut Rng, ours: &str) -> String {
    let list: Vec<String> = (0..rng.next() % 6).map(|_| name(rng)).collect();
    let list = list.join(" ");
    let sub = rng.pick(&["LS", "LS *", "ACK", "NAK", "NEW", "DEL", "LIST", ""]);
    match rng.next() % 12 {
        0..=5 => format!(":s CAP * {sub} :{list}"),
        6 => format!(":s CAP * {sub}"),
        7 => {
            let refused = if rng.next().is_multiple_of(2) {
                ours.to_owned()
            } else {
                name(rng)
            };
            format!(":s {} * {refused} :x", rng.pick(&["433", "432", "437"]))
        }
        8 => format!("AUTHENTICATE {}", rng.pick(&["+", "*", "\0", "x"])),
        9 => format!(
            ":s {} * {} :{list}",
            rng.pick(&["900", "902", "903", "904", "905", "906", "907", "908"]),
            name(rng)
        ),
        10 => format!(":s 421 * CAP :{list}"),
        _ => match rng.next() % 4 {
            0 => format!(":s 001 {} :Welcome", name(rng)),
            1 => format!("ERROR :{list}"),
            _ => format!(":s NOTICE * :{list}"),
        },
    }
}

#[test]
fn generated_server_lines_never_panic_or_send_a_bad_line() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut seen = std::collections::BTreeSet::new();
    for run in 0..2000 {
        let mut s = setup("textual");
        s.alternates = vec!["textual2".into()];
        if run % 2 == 0 {
            s.auth = Auth::SaslPlain;
            s.account_password = Some(Secret::new("pw"));
        }
        let mut out = Output::default();
        let mut reg = Registration::start(s, WANTED, &mut out).unwrap();
        let mut fed = Vec::new();
        for _ in 0..40 {
            let raw = server_line(&mut rng, reg.nick());
            let Ok(msg) = Message::parse(&raw) else {
                continue;
            };
            fed.push(raw);
            let progress = reg.step(&msg, &mut out);
            for line in out.lines.drain(..) {
                let text = line.as_str();
                let body = text.strip_suffix("\r\n").unwrap();
                seen.insert(body.split(' ').take(2).collect::<Vec<_>>().join(" "));
                assert!(
                    text.len() <= 512 && !body.contains(['\r', '\n', '\0']),
                    "run {run} sent {text:?} after {fed:#?}"
                );
            }
            match progress {
                Progress::Registering(next) => reg = next,
                _ => break,
            }
        }
    }
    for kind in ["CAP REQ", "CAP END", "AUTHENTICATE PLAIN", "NICK textual2"] {
        assert!(seen.contains(kind), "never sent {kind}: {seen:?}");
    }
}
