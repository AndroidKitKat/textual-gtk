//! Registration: everything between the socket opening and 001. It lives in
//! `Link::Registering` and is consumed by `step`, so the secrets it holds go
//! away when registration ends, however it ends.

use std::fmt;

use crate::irc::caps::Caps;
use crate::irc::casemap::CaseMapping;
use crate::irc::sasl::{Mechanism, SaslFailure, SaslOutcome, SaslState};
use crate::irc::{Message, WireLine};
use crate::secret::Secret;

/// How we identify to the network. Explicit, unlike Textual, which infers
/// SASL PLAIN from a NickServ password being set. Phase 3 adds SaslExternal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Auth {
    #[default]
    None,
    SaslPlain,
    /// IDENTIFY after 001, which is the session's job, not registration's.
    NickServ,
}

/// RFC 1459's NICKLEN, the padding limit while the real one is unknown
/// (005 arrives after 001).
const DEFAULT_NICKLEN: usize = 9;

#[derive(Debug, Clone)]
pub struct Setup {
    pub nick: String,
    pub alternates: Vec<String>,
    pub username: String,
    pub realname: String,
    pub auth: Auth,
    pub server_password: Option<Secret>,
    /// The SASL password for `Auth::SaslPlain`.
    pub account_password: Option<Secret>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupError {
    BadNick(String),
    BadUsername(String),
    BadRealname,
    BadServerPassword,
}

impl fmt::Display for SetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetupError::BadNick(n) => write!(f, "{n:?} cannot be used as a nick"),
            SetupError::BadUsername(u) => write!(f, "{u:?} cannot be used as a user name"),
            SetupError::BadRealname => f.write_str("the real name contains a line break"),
            SetupError::BadServerPassword => {
                f.write_str("the server password contains a line break")
            }
        }
    }
}

impl std::error::Error for SetupError {}

/// What registration sends and tells the console. Lines go out in order.
#[derive(Debug, Default)]
pub struct Output {
    pub lines: Vec<WireLine>,
    pub notices: Vec<Notice>,
}

/// Something the console should say. Every server line is also printed as
/// usual; these explain what registration did about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    NickRejected {
        nick: String,
        reason: NickRejection,
        trying: String,
    },
    LoggedIn {
        account: Option<String>,
    },
    SaslFailed(SaslFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NickRejection {
    InUse,
    Erroneous,
    Unavailable,
}

impl fmt::Display for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Notice::NickRejected {
                nick,
                reason,
                trying,
            } => {
                let why = match reason {
                    NickRejection::InUse => "is already in use",
                    NickRejection::Erroneous => "was refused by the server",
                    NickRejection::Unavailable => "is temporarily unavailable",
                };
                write!(f, "The nick {nick} {why}; trying {trying}")
            }
            Notice::LoggedIn { account: Some(a) } => write!(f, "SASL: logged in as {a}"),
            Notice::LoggedIn { account: None } => f.write_str("SASL: logged in"),
            Notice::SaslFailed(why) => write!(f, "SASL failed: {why}"),
        }
    }
}

// Registration is the large variant and the common one: it is moved once per
// line, only until 001, so boxing it would add an allocation for nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Progress {
    Registering(Registration),
    Registered(Registered),
    Failed(Failure),
}

/// The fruit of 001, for building the `Session`.
#[derive(Debug)]
pub struct Registered {
    /// The nick the server confirmed in 001.
    pub nick: String,
    /// Still live: `CAP NEW` and `DEL` keep arriving after registration.
    pub caps: Caps,
    /// The account SASL logged in to, when 900 named it.
    pub account: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Every nick, alternate and padded form was rejected.
    NoUsableNick { tried: Vec<String> },
    /// The server sent ERROR before 001.
    ServerError(String),
}

#[derive(Debug)]
pub struct Registration {
    caps: Caps,
    phase: Phase,
    nicks: NickLadder,
    auth: Auth,
    username: String,
    account_password: Option<Secret>,
    account: Option<String>,
}

#[derive(Debug)]
enum Phase {
    /// Waiting for LS to finish and every REQ to be answered.
    Negotiating,
    Authenticating(SaslState),
    /// CAP END sent (or the server never spoke CAP); waiting for 001.
    Ended,
}

impl Registration {
    /// Sends CAP LS 302, PASS, NICK and USER. `wanted` is the capability
    /// list (normally `caps::WANTED`); `sasl` is added when `auth` uses it.
    pub fn start(setup: Setup, wanted: &[&str], out: &mut Output) -> Result<Self, SetupError> {
        let nick = wire_nick(&setup.nick)?;
        for alt in &setup.alternates {
            wire_nick(alt)?;
        }
        if !is_atom(&setup.username) {
            return Err(SetupError::BadUsername(setup.username));
        }
        let user = Message::new("USER", &[&setup.username, "0", "*", &setup.realname])
            .to_wire()
            .map_err(|_| SetupError::BadRealname)?;
        let pass = match &setup.server_password {
            Some(pw) => Some(
                Message::new("PASS", &[pw.expose()])
                    .to_wire()
                    .map_err(|_| SetupError::BadServerPassword)?,
            ),
            None => None,
        };

        let sasl = setup.auth == Auth::SaslPlain;
        let caps = Caps::new(wanted.iter().copied().chain(sasl.then_some("sasl")));
        out.lines.push(Caps::ls_request());
        out.lines.extend(pass);
        out.lines.push(nick);
        out.lines.push(user);

        Ok(Registration {
            caps,
            phase: Phase::Negotiating,
            nicks: NickLadder::new(setup.nick, setup.alternates),
            auth: setup.auth,
            username: setup.username,
            account_password: setup.account_password,
            account: None,
        })
    }

    pub fn nick(&self) -> &str {
        &self.nicks.current
    }

    /// Whether `msg` refuses the nick the ladder last sent. A refusal of a
    /// nick the user typed is the user's to read, and moves nothing.
    pub fn is_ladder_rejection(&self, msg: &Message) -> bool {
        matches!(msg.command.as_str(), "433" | "432" | "437")
            && msg
                .param(1)
                .is_none_or(|n| CaseMapping::Rfc1459.same_nick(n, &self.nicks.current))
    }

    /// Takes one server line. Lines registration has no use for (NOTICE,
    /// 020, PING) leave it unchanged.
    pub fn step(mut self, msg: &Message, out: &mut Output) -> Progress {
        match msg.command.as_str() {
            "001" => {
                if self.auth == Auth::SaslPlain && matches!(self.phase, Phase::Negotiating) {
                    out.notices
                        .push(Notice::SaslFailed(SaslFailure::NotOffered));
                }
                return Progress::Registered(Registered {
                    nick: msg.param(0).unwrap_or(&self.nicks.current).to_owned(),
                    caps: self.caps,
                    account: self.account,
                });
            }
            "ERROR" => {
                let text = msg.params.last().cloned().unwrap_or_default();
                return Progress::Failed(Failure::ServerError(text));
            }
            "433" | "432" | "437" if self.is_ladder_rejection(msg) => {
                let reason = match msg.command.as_str() {
                    "433" => NickRejection::InUse,
                    "432" => NickRejection::Erroneous,
                    _ => NickRejection::Unavailable,
                };
                let rejected = msg.param(1).unwrap_or(&self.nicks.current).to_owned();
                match self.nicks.reject(reason) {
                    Some(next) => {
                        out.lines
                            .push(wire_nick(&next).expect("ladder nicks are valid"));
                        out.notices.push(Notice::NickRejected {
                            nick: rejected,
                            reason,
                            trying: next,
                        });
                    }
                    None => {
                        return Progress::Failed(Failure::NoUsableNick {
                            tried: self.nicks.tried,
                        });
                    }
                }
            }
            // A NICK the user typed, if the server confirms it before 001.
            "NICK"
                if msg.source().is_some_and(|s| {
                    CaseMapping::Rfc1459.same_nick(s.nick, &self.nicks.current)
                }) =>
            {
                if let Some(new) = msg.param(0) {
                    new.clone_into(&mut self.nicks.current);
                }
            }
            "CAP" => {
                let step = self.caps.on_cap(msg);
                out.lines.extend(step.send);
                if matches!(self.phase, Phase::Negotiating) && self.caps.settled() {
                    self.after_caps(out);
                }
            }
            _ => {
                if let Phase::Authenticating(sasl) = &mut self.phase
                    && let Some(outcome) = sasl.on_message(msg, &mut out.lines)
                {
                    match outcome {
                        SaslOutcome::LoggedIn { account } => {
                            self.account.clone_from(&account);
                            out.notices.push(Notice::LoggedIn { account });
                        }
                        SaslOutcome::Failed(why) => out.notices.push(Notice::SaslFailed(why)),
                    }
                    self.end_caps(out);
                }
            }
        }
        Progress::Registering(self)
    }

    fn after_caps(&mut self, out: &mut Output) {
        if self.auth != Auth::SaslPlain {
            return self.end_caps(out);
        }
        let failure = match self.caps.offered_value("sasl") {
            _ if !self.caps.is_enabled("sasl") => SaslFailure::NotOffered,
            offered => match SaslState::begin(
                &[Mechanism::Plain],
                &self.username,
                self.account_password.take(),
                offered.unwrap_or(""),
                &mut out.lines,
            ) {
                Ok(sasl) => {
                    self.phase = Phase::Authenticating(sasl);
                    return;
                }
                Err(why) => why,
            },
        };
        out.notices.push(Notice::SaslFailed(failure));
        self.end_caps(out);
    }

    fn end_caps(&mut self, out: &mut Output) {
        out.lines.push(Caps::end_request());
        self.phase = Phase::Ended;
    }
}

/// The nicks to try before 001: the configured one, the alternates, then
/// the configured one padded with `_`.
#[derive(Debug)]
struct NickLadder {
    current: String,
    tried: Vec<String>,
    alternates: std::vec::IntoIter<String>,
    /// The longest nick the server rejected only for being taken, which
    /// proves it accepts that length.
    longest_valid: usize,
}

impl NickLadder {
    fn new(nick: String, alternates: Vec<String>) -> Self {
        NickLadder {
            current: nick.clone(),
            tried: vec![nick],
            alternates: alternates.into_iter(),
            longest_valid: 0,
        }
    }

    fn reject(&mut self, reason: NickRejection) -> Option<String> {
        if reason != NickRejection::Erroneous {
            self.longest_valid = self.longest_valid.max(self.current.chars().count());
        }
        let next = self
            .alternates
            .by_ref()
            .find(|n| !self.tried.contains(n))
            .or_else(|| self.padded())?;
        self.tried.push(next.clone());
        self.current.clone_from(&next);
        Some(next)
    }

    /// The next padded form of the configured nick. Underscores are appended
    /// while the result fits the limit; a nick already at the limit gets one
    /// try with its last character replaced.
    fn padded(&self) -> Option<String> {
        let limit = DEFAULT_NICKLEN.max(self.longest_valid);
        let base: Vec<char> = self.tried[0].chars().collect();
        let mut candidates = (1..)
            .map(|k| base.len() + k)
            .take_while(|len| *len <= limit)
            .map(|len| {
                let mut n: String = base.iter().collect();
                n.extend(std::iter::repeat_n('_', len - base.len()));
                n
            })
            .peekable();
        if candidates.peek().is_none() {
            let mut n: String = base.iter().take(limit - 1).collect();
            n.push('_');
            return Some(n).filter(|n| !self.tried.contains(n));
        }
        candidates.find(|n| !self.tried.contains(n))
    }
}

fn is_atom(s: &str) -> bool {
    !s.is_empty() && !s.starts_with(':') && !s.contains([' ', '\r', '\n', '\0'])
}

fn wire_nick(nick: &str) -> Result<WireLine, SetupError> {
    if !is_atom(nick) {
        return Err(SetupError::BadNick(nick.to_owned()));
    }
    Message::new("NICK", &[nick])
        .to_wire()
        .map_err(|_| SetupError::BadNick(nick.to_owned()))
}
