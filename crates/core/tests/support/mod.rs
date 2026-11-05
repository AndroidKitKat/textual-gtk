//! The `Script` harness and the transcript runner. They live here, beside
//! the integration tests, rather than behind a feature in the library: they
//! see only the public API, so a scenario can only assert what a caller can
//! observe (lines sent, notices, the outcome).
//!
//! `Script` drives `Registration` alone; the transcript runner drives a
//! whole `Network` (`transcript.rs`).

#![allow(dead_code)]

mod transcript;

use std::collections::VecDeque;

use textual_core::irc::Message;
use textual_core::irc::caps::WANTED;
use textual_core::model::registration::{Auth, Notice, Output, Progress, Registration, Setup};

#[allow(unused_imports)]
pub use transcript::{run_transcript, run_transcript_hiding};

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
