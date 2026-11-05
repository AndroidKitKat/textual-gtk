//! The `Script` harness and the transcript runner. They live here, beside
//! the integration tests, rather than behind a feature in the library: they
//! see only the public API, so a scenario can only assert what a caller can
//! observe (lines sent, notices, the outcome).
//!
//! Until unit 4 adds `Link`, a script drives `Registration` alone.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::fmt::Write as _;

use textual_core::irc::Message;
use textual_core::irc::caps::WANTED;
use textual_core::model::registration::{
    Auth, Failure, Notice, Output, Progress, Registration, Setup,
};
use textual_core::secret::Secret;

pub fn setup(nick: &str) -> Setup {
    Setup {
        nick: nick.to_owned(),
        alternates: Vec::new(),
        username: nick.to_owned(),
        realname: "Textual".to_owned(),
        auth: Auth::None,
        server_password: None,
        account_password: None,
    }
}

pub struct Script {
    state: Option<Progress>,
    sent: VecDeque<String>,
    notices: VecDeque<Notice>,
}

impl Script {
    pub fn new(setup: Setup) -> Self {
        Script::with_caps(setup, WANTED)
    }

    pub fn with_caps(setup: Setup, wanted: &[&str]) -> Self {
        let mut out = Output::default();
        let reg = Registration::start(setup, wanted, &mut out).expect("valid setup");
        let mut script = Script {
            state: Some(Progress::Registering(reg)),
            sent: VecDeque::new(),
            notices: VecDeque::new(),
        };
        script.collect(out);
        script
    }

    fn collect(&mut self, out: Output) {
        for line in out.lines {
            let text = line.as_str();
            let text = text.strip_suffix("\r\n").expect("wire lines end in CRLF");
            self.sent.push_back(text.to_owned());
        }
        self.notices.extend(out.notices);
    }

    /// Feeds one server line, which must parse.
    pub fn recv(&mut self, raw: &str) {
        let msg = Message::parse(raw).unwrap_or_else(|e| panic!("bad line {raw:?}: {e}"));
        let state = self.state.take().expect("state is always put back");
        let next = match state {
            Progress::Registering(reg) => {
                let mut out = Output::default();
                let progress = reg.step(&msg, &mut out);
                self.collect(out);
                progress
            }
            done => panic!("line {raw:?} after registration ended: {done:?}"),
        };
        self.state = Some(next);
    }

    /// Takes the oldest unchecked sent line, if any.
    pub fn next_sent(&mut self) -> Option<String> {
        self.sent.pop_front()
    }

    pub fn next_notice(&mut self) -> Option<Notice> {
        self.notices.pop_front()
    }

    /// Asserts these lines, and no others, went out since the last check.
    pub fn sent<const N: usize>(&mut self, lines: [&str; N]) {
        let got: Vec<String> = self.sent.drain(..).collect();
        assert_eq!(got, lines, "lines sent");
    }

    pub fn sent_nothing(&mut self) {
        self.sent::<0>([]);
    }

    pub fn state(&self) -> &Progress {
        self.state.as_ref().expect("state is always put back")
    }
}

/// Runs one transcript and returns every mismatch, each with its line
/// number. The DSL:
///
/// - `= key value` configures the client before the first exchange (`nick`,
///   `alternates`, `user`, `realname`, `auth none|sasl-plain|nickserv`,
///   `server-password`, `account-password`, `caps` to replace the wanted
///   list).
/// - `> line`: the next line the client sent.
/// - `< line`: a line from the server. Every sent line must have been
///   checked before it.
/// - `? notice text`: the next console notice, as displayed.
/// - `? registered nick [account]`, `? failed no-usable-nick`,
///   `? failed error text` or `? registering`. Every transcript must end
///   with one of these, so a truncated one cannot pass.
/// - `@ +Ns` (virtual time) is rejected until a script drives a `Link`.
/// - `#` starts a comment; blank lines are skipped.
pub fn run_transcript(source: &str) -> Result<(), String> {
    let mut config = setup("textual");
    let mut wanted: Option<Vec<String>> = None;
    let mut script: Option<Script> = None;
    let mut errors = String::new();
    let mut ends_in_state = false;

    for (index, raw) in source.lines().enumerate() {
        let n = index + 1;
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (kind, rest) = line.split_at(1);
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        if kind == "=" {
            if script.is_some() {
                return Err(format!("{n}: configuration after the first exchange"));
            }
            let (key, value) = rest.split_once(' ').unwrap_or((rest, ""));
            match key {
                "nick" => value.clone_into(&mut config.nick),
                "alternates" => {
                    config.alternates = value.split_whitespace().map(str::to_owned).collect();
                }
                "user" => value.clone_into(&mut config.username),
                "realname" => value.clone_into(&mut config.realname),
                "auth" => {
                    config.auth = match value {
                        "none" => Auth::None,
                        "sasl-plain" => Auth::SaslPlain,
                        "nickserv" => Auth::NickServ,
                        _ => return Err(format!("{n}: unknown auth {value:?}")),
                    };
                }
                "server-password" => config.server_password = Some(Secret::new(value)),
                "account-password" => config.account_password = Some(Secret::new(value)),
                "caps" => wanted = Some(value.split_whitespace().map(str::to_owned).collect()),
                _ => return Err(format!("{n}: unknown setting {key:?}")),
            }
            continue;
        }

        let t = script.get_or_insert_with(|| {
            let caps: Vec<&str> = match &wanted {
                Some(list) => list.iter().map(String::as_str).collect(),
                None => WANTED.to_vec(),
            };
            Script::with_caps(config.clone(), &caps)
        });
        ends_in_state = kind == "?" && is_end_state(rest);
        match kind {
            ">" => match t.next_sent() {
                Some(got) if got == rest => {}
                Some(got) => writeln!(errors, "{n}: expected sent {rest:?}, got {got:?}").unwrap(),
                None => writeln!(errors, "{n}: expected sent {rest:?}, nothing was sent").unwrap(),
            },
            "<" => {
                while let Some(extra) = t.next_sent() {
                    writeln!(errors, "{n}: unexpected sent {extra:?}").unwrap();
                }
                if !matches!(t.state(), Progress::Registering(_)) {
                    writeln!(errors, "{n}: server line after the end: {:?}", t.state()).unwrap();
                    break;
                }
                t.recv(rest);
            }
            "?" => check_query(t, rest, n, &mut errors),
            "@" => return Err(format!("{n}: virtual time needs a Link (unit 4)")),
            _ => return Err(format!("{n}: unknown line kind {kind:?}")),
        }
    }

    if let Some(t) = &mut script {
        while let Some(extra) = t.next_sent() {
            writeln!(errors, "end: unexpected sent {extra:?}").unwrap();
        }
        while let Some(extra) = t.next_notice() {
            writeln!(errors, "end: unchecked notice {:?}", extra.to_string()).unwrap();
        }
    }
    if !ends_in_state {
        errors.push_str("end: no end state (`? registered`, `? failed` or `? registering`)\n");
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn is_end_state(query: &str) -> bool {
    let what = query.split(' ').next().unwrap_or("");
    matches!(what, "registered" | "failed" | "registering")
}

fn check_query(t: &mut Script, query: &str, n: usize, errors: &mut String) {
    let (what, arg) = query.split_once(' ').unwrap_or((query, ""));
    let mismatch = match what {
        "notice" => match t.next_notice() {
            Some(notice) if notice.to_string() == arg => None,
            Some(notice) => Some(format!("notice {:?}", notice.to_string())),
            None => Some("no notice".to_owned()),
        },
        "registered" => {
            let mut words = arg.split(' ');
            let nick = words.next().unwrap_or("");
            let account = words.next();
            match t.state() {
                Progress::Registered(r) if r.nick == nick && r.account.as_deref() == account => {
                    None
                }
                other => Some(format!("{other:?}")),
            }
        }
        "failed" => match (arg.split_once(' ').unwrap_or((arg, "")), t.state()) {
            (("no-usable-nick", _), Progress::Failed(Failure::NoUsableNick { .. })) => None,
            (("error", text), Progress::Failed(Failure::ServerError(got))) if got == text => None,
            (_, other) => Some(format!("{other:?}")),
        },
        "registering" => match t.state() {
            Progress::Registering(_) => None,
            other => Some(format!("{other:?}")),
        },
        _ => Some(format!("unknown query {what:?}")),
    };
    if let Some(got) = mismatch {
        writeln!(errors, "{n}: expected {query:?}, got {got}").unwrap();
    }
}
