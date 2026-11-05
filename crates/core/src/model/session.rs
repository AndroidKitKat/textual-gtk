//! What we know about ourselves on a registered connection. A `Session`
//! exists only inside a live `Link` variant, so a disconnect drops all of it.

use crate::irc::caps::Caps;
use crate::irc::casemap::CaseMapping;
use crate::irc::isupport::Isupport;
use crate::irc::{Message, Source, WireLine};
use crate::model::registration::Registered;

#[derive(Debug)]
pub struct Session {
    nick: String,
    /// `user@host` as the server shows us to others, once a line from us
    /// has carried it.
    hostmask: Option<String>,
    account: Option<String>,
    caps: Caps,
    isupport: Isupport,
}

impl Session {
    pub fn new(registered: Registered) -> Self {
        Session {
            nick: registered.nick,
            hostmask: None,
            account: registered.account,
            caps: registered.caps,
            isupport: Isupport::default(),
        }
    }

    pub fn nick(&self) -> &str {
        &self.nick
    }

    pub fn hostmask(&self) -> Option<&str> {
        self.hostmask.as_deref()
    }

    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    pub fn caps(&self) -> &Caps {
        &self.caps
    }

    pub fn isupport(&self) -> &Isupport {
        &self.isupport
    }

    pub fn casemap(&self) -> CaseMapping {
        self.isupport.casemapping
    }

    /// Takes one server line after 001. Lines that need an answer push it
    /// to `send`.
    pub fn handle(&mut self, msg: &Message, send: &mut Vec<WireLine>) {
        if let Some(source) = msg.source()
            && self.is_me(source.nick)
        {
            self.learn_hostmask(source);
        }
        match msg.command.as_str() {
            "005" if msg.params.len() > 2 => {
                let tokens: Vec<&str> = msg.params[1..msg.params.len() - 1]
                    .iter()
                    .map(String::as_str)
                    .collect();
                self.isupport.apply(&tokens);
            }
            "NICK" => {
                if let (Some(source), Some(new)) = (msg.source(), msg.param(0))
                    && self.is_me(source.nick)
                {
                    new.clone_into(&mut self.nick);
                }
            }
            "CAP" => send.extend(self.caps.on_cap(msg).send),
            _ => {}
        }
    }

    fn is_me(&self, nick: &str) -> bool {
        self.casemap().same_nick(nick, &self.nick)
    }

    fn learn_hostmask(&mut self, source: Source<'_>) {
        if let (Some(user), Some(host)) = (source.user, source.host) {
            self.hostmask = Some(format!("{user}@{host}"));
        }
    }
}
