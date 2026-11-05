//! SASL authentication during registration, as a step function over the
//! AUTHENTICATE lines and the 90x numerics.

use std::fmt;

use super::{Message, WireLine};
use crate::secret::Secret;

/// AUTHENTICATE payloads go out in chunks of this many base64 bytes.
const CHUNK: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    Plain,
    /// The TLS client certificate identifies us. Phase 3 adds the
    /// certificate and an `Auth` that selects this.
    External,
}

impl Mechanism {
    pub fn name(self) -> &'static str {
        match self {
            Mechanism::Plain => "PLAIN",
            Mechanism::External => "EXTERNAL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaslOutcome {
    /// 903 or 907. `account` comes from 900 when the server sent one.
    LoggedIn {
        account: Option<String>,
    },
    Failed(SaslFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaslFailure {
    /// The server did not offer the `sasl` capability.
    NotOffered,
    /// The server lists its mechanisms and none of ours is among them.
    NoMechanism { offered: Vec<String> },
    /// SASL is configured but no password was available.
    NoPassword,
    /// 902, 904, 905 or 906, with the server's text.
    Server { numeric: u16, text: String },
}

impl fmt::Display for SaslFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaslFailure::NotOffered => f.write_str("the server does not offer SASL"),
            SaslFailure::NoMechanism { offered } => write!(
                f,
                "the server offers no mechanism we support (it offers {})",
                offered.join(", ")
            ),
            SaslFailure::NoPassword => f.write_str("no password is stored for this account"),
            SaslFailure::Server { text, .. } => f.write_str(text),
        }
    }
}

pub struct SaslState {
    username: String,
    password: Option<Secret>,
    /// Usable mechanisms in preference order; the first is in progress.
    candidates: Vec<Mechanism>,
    /// The list from 908, which arrives just before the 904 it explains.
    server_list: Option<Vec<String>>,
    retried: bool,
    account: Option<String>,
}

impl fmt::Debug for SaslState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SaslState")
            .field("username", &self.username)
            .field("candidates", &self.candidates)
            .finish_non_exhaustive()
    }
}

impl SaslState {
    /// Starts with the first usable mechanism, or fails without sending
    /// anything. `offered` is the `sasl` capability's value from CAP LS: a
    /// comma-separated list, or empty when the server did not say.
    pub fn begin(
        preference: &[Mechanism],
        username: &str,
        password: Option<Secret>,
        offered: &str,
        send: &mut Vec<WireLine>,
    ) -> Result<SaslState, SaslFailure> {
        let usable: Vec<Mechanism> = preference
            .iter()
            .copied()
            .filter(|m| *m != Mechanism::Plain || password.is_some())
            .collect();
        if usable.is_empty() {
            return Err(SaslFailure::NoPassword);
        }
        let offered: Vec<String> = offered
            .split(',')
            .filter(|m| !m.is_empty())
            .map(str::to_owned)
            .collect();
        let candidates: Vec<Mechanism> = if offered.is_empty() {
            usable
        } else {
            usable
                .into_iter()
                .filter(|m| offered.iter().any(|o| o.eq_ignore_ascii_case(m.name())))
                .collect()
        };
        let Some(first) = candidates.first() else {
            return Err(SaslFailure::NoMechanism { offered });
        };
        send.push(authenticate(first.name()));
        Ok(SaslState {
            username: username.to_owned(),
            password,
            candidates,
            server_list: None,
            retried: false,
            account: None,
        })
    }

    /// Takes AUTHENTICATE and 900-908; returns the outcome once there is one.
    pub fn on_message(&mut self, msg: &Message, send: &mut Vec<WireLine>) -> Option<SaslOutcome> {
        let text = || msg.params.last().cloned().unwrap_or_default();
        match msg.command.as_str() {
            "AUTHENTICATE" => {
                if msg.param(0) == Some("+") {
                    send.extend(self.response().into_iter().map(|c| authenticate(&c)));
                } else {
                    // Neither mechanism we speak has a server challenge.
                    send.push(authenticate("*"));
                }
                None
            }
            "900" => {
                self.account = msg.param(2).map(str::to_owned);
                None
            }
            "903" | "907" => Some(SaslOutcome::LoggedIn {
                account: self.account.take(),
            }),
            "908" => {
                let list = msg.param(1).unwrap_or("");
                self.server_list = Some(list.split(',').map(str::to_owned).collect());
                None
            }
            "904" if self.retry(send) => None,
            "902" | "904" | "905" | "906" => Some(SaslOutcome::Failed(SaslFailure::Server {
                numeric: msg.command.parse().unwrap_or_default(),
                text: text(),
            })),
            _ => None,
        }
    }

    /// One retry, with the next of our mechanisms that 908 listed.
    fn retry(&mut self, send: &mut Vec<WireLine>) -> bool {
        let Some(list) = self.server_list.take() else {
            return false;
        };
        if self.retried {
            return false;
        }
        let next = self.candidates[1..]
            .iter()
            .position(|m| list.iter().any(|o| o.eq_ignore_ascii_case(m.name())));
        let Some(index) = next else {
            return false;
        };
        self.candidates.drain(..=index);
        self.retried = true;
        send.push(authenticate(self.candidates[0].name()));
        true
    }

    fn response(&self) -> Vec<String> {
        let payload = match self.candidates[0] {
            Mechanism::Plain => {
                let password = self.password.as_ref().map_or("", Secret::expose);
                let user = &self.username;
                format!("{user}\0{user}\0{password}").into_bytes()
            }
            Mechanism::External => Vec::new(),
        };
        chunks(&base64(&payload))
    }
}

fn authenticate(arg: &str) -> WireLine {
    Message::new("AUTHENTICATE", &[arg])
        .to_wire()
        .expect("AUTHENTICATE arguments are mechanism names or base64")
}

/// Splits a base64 payload into AUTHENTICATE arguments. An empty payload is
/// `+`, and so is the line after a final chunk of exactly 400 bytes, so the
/// server knows the payload ended.
fn chunks(encoded: &str) -> Vec<String> {
    let mut out: Vec<String> = encoded
        .as_bytes()
        .chunks(CHUNK)
        .map(|c| String::from_utf8(c.to_vec()).expect("base64 is ASCII"))
        .collect();
    if out.last().is_none_or(|c| c.len() == CHUNK) {
        out.push("+".to_owned());
    }
    out
}

fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for group in input.chunks(3) {
        let b = [
            group[0],
            *group.get(1).unwrap_or(&0),
            *group.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= group.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(raw: &str) -> Message {
        Message::parse(raw).unwrap()
    }

    fn args(lines: &[WireLine]) -> Vec<&str> {
        lines
            .iter()
            .map(|l| {
                let l = l.as_str().trim_end_matches("\r\n");
                l.strip_prefix("AUTHENTICATE ").unwrap_or(l)
            })
            .collect()
    }

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, encoded) in vectors {
            assert_eq!(base64(plain.as_bytes()), encoded);
        }
    }

    fn plain_with_password(password: &str) -> Vec<String> {
        let mut send = Vec::new();
        let mut sasl = SaslState::begin(
            &[Mechanism::Plain],
            "u",
            Some(Secret::new(password)),
            "",
            &mut send,
        )
        .unwrap();
        send.clear();
        sasl.on_message(&line("AUTHENTICATE +"), &mut send);
        args(&send).into_iter().map(str::to_owned).collect()
    }

    #[test]
    fn payload_of_exactly_400_bytes_ends_with_plus() {
        // "u\0u\0" is 4 bytes; 296 more make 300, which encodes to 400.
        let out = plain_with_password(&"p".repeat(296));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), 400);
        assert_eq!(out[1], "+");
    }

    #[test]
    fn longer_payload_is_split_without_a_trailing_plus() {
        let out = plain_with_password(&"p".repeat(400));
        assert_eq!(out.iter().map(String::len).collect::<Vec<_>>(), [400, 140]);
    }

    #[test]
    fn external_sends_an_empty_response() {
        let mut send = Vec::new();
        let mut sasl =
            SaslState::begin(&[Mechanism::External], "u", None, "EXTERNAL", &mut send).unwrap();
        assert_eq!(args(&send), ["EXTERNAL"]);
        send.clear();
        sasl.on_message(&line("AUTHENTICATE +"), &mut send);
        assert_eq!(args(&send), ["+"]);
    }

    #[test]
    fn a_908_listing_another_mechanism_gets_one_retry() {
        let mut send = Vec::new();
        let prefs = [Mechanism::External, Mechanism::Plain];
        let mut sasl =
            SaslState::begin(&prefs, "u", Some(Secret::new("pw")), "", &mut send).unwrap();
        assert_eq!(args(&send), ["EXTERNAL"]);
        send.clear();
        let m908 = line(":s 908 * PLAIN :are available SASL mechanisms");
        let m904 = line(":s 904 * :SASL authentication failed");
        assert_eq!(sasl.on_message(&m908, &mut send), None);
        assert_eq!(sasl.on_message(&m904, &mut send), None);
        assert_eq!(args(&send), ["PLAIN"]);
        send.clear();
        assert_eq!(sasl.on_message(&m908, &mut send), None);
        assert!(matches!(
            sasl.on_message(&m904, &mut send),
            Some(SaslOutcome::Failed(SaslFailure::Server {
                numeric: 904,
                ..
            }))
        ));
        assert!(send.is_empty(), "no second retry");
    }

    #[test]
    fn debug_does_not_print_the_password() {
        let mut send = Vec::new();
        let sasl = SaslState::begin(
            &[Mechanism::Plain],
            "u",
            Some(Secret::new("hunter2")),
            "",
            &mut send,
        )
        .unwrap();
        assert!(!format!("{sasl:?}").contains("hunter2"));
    }
}
