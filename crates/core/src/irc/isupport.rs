//! What a server advertises in RPL_ISUPPORT (005), parsed once into the
//! questions the rest of the client asks: is this a channel name, which
//! modes are member prefixes and in what rank, which modes take a parameter.

use super::casemap::CaseMapping;

/// Textual's defaults (`IRCISupportInfo reset`), used until 005 says
/// otherwise and again after a token is negated.
const DEFAULT_PREFIX: &[(char, char)] = &[('o', '@'), ('v', '+')];
const DEFAULT_CHANTYPES: &str = "#&";
const DEFAULT_CHANMODES: [&str; 4] = ["beI", "k", "l", "imnpst"];

#[derive(Debug, Clone, PartialEq)]
pub struct Isupport {
    pub network: Option<String>,
    pub casemapping: CaseMapping,
    pub nicklen: Option<usize>,
    pub whox: bool,
    /// The MONITOR limit, if the server supports MONITOR.
    pub monitor: Option<usize>,
    /// (mode, symbol) pairs, highest rank first.
    prefix: Vec<(char, char)>,
    /// CHANMODES groups A (list), B (always a parameter), C (parameter when
    /// set), D (never a parameter).
    chanmodes: [String; 4],
    chantypes: String,
    statusmsg: String,
}

impl Default for Isupport {
    fn default() -> Self {
        Isupport {
            network: None,
            casemapping: CaseMapping::default(),
            nicklen: None,
            whox: false,
            monitor: None,
            prefix: DEFAULT_PREFIX.to_vec(),
            chanmodes: DEFAULT_CHANMODES.map(str::to_owned),
            chantypes: DEFAULT_CHANTYPES.to_owned(),
            statusmsg: String::new(),
        }
    }
}

impl Isupport {
    /// Merges one 005 line. `tokens` are the parameters between our nick and
    /// the trailing "are supported by this server" text. Tokens accumulate
    /// across lines; `-TOKEN` goes back to the default.
    pub fn apply(&mut self, tokens: &[&str]) {
        let defaults = Isupport::default();
        for token in tokens {
            if let Some(name) = token.strip_prefix('-') {
                self.reset(name, &defaults);
                continue;
            }
            let (name, value) = token.split_once('=').unwrap_or((token, ""));
            let value = unescape(value);
            match name.to_ascii_uppercase().as_str() {
                "NETWORK" if !value.is_empty() => self.network = Some(value),
                "CASEMAPPING" => {
                    if let Some(mapping) = CaseMapping::from_token(&value) {
                        self.casemapping = mapping;
                    }
                }
                "NICKLEN" => self.nicklen = value.parse().ok(),
                "WHOX" => self.whox = true,
                "MONITOR" => self.monitor = Some(value.parse().unwrap_or(usize::MAX)),
                "PREFIX" => self.prefix = parse_prefix(&value),
                "CHANMODES" => {
                    let mut groups = value.split(',').map(str::to_owned);
                    for slot in &mut self.chanmodes {
                        *slot = groups.next().unwrap_or_default();
                    }
                }
                "CHANTYPES" => self.chantypes = value,
                "STATUSMSG" => self.statusmsg = value,
                _ => {}
            }
        }
    }

    fn reset(&mut self, name: &str, defaults: &Isupport) {
        match name.to_ascii_uppercase().as_str() {
            "NETWORK" => self.network = None,
            "CASEMAPPING" => self.casemapping = defaults.casemapping,
            "NICKLEN" => self.nicklen = None,
            "WHOX" => self.whox = false,
            "MONITOR" => self.monitor = None,
            "PREFIX" => self.prefix = defaults.prefix.clone(),
            "CHANMODES" => self.chanmodes = defaults.chanmodes.clone(),
            "CHANTYPES" => self.chantypes = defaults.chantypes.clone(),
            "STATUSMSG" => self.statusmsg = String::new(),
            _ => {}
        }
    }

    /// Member prefixes as (mode, symbol), highest rank first.
    pub fn prefix(&self) -> &[(char, char)] {
        &self.prefix
    }

    /// 0 is the highest rank (usually `q` or `o`).
    pub fn rank_of_mode(&self, mode: char) -> Option<usize> {
        self.prefix.iter().position(|(m, _)| *m == mode)
    }

    pub fn mode_of_symbol(&self, symbol: char) -> Option<char> {
        self.prefix
            .iter()
            .find(|(_, s)| *s == symbol)
            .map(|(m, _)| *m)
    }

    pub fn symbol_of_mode(&self, mode: char) -> Option<char> {
        self.prefix
            .iter()
            .find(|(m, _)| *m == mode)
            .map(|(_, s)| *s)
    }

    pub fn is_channel(&self, name: &str) -> bool {
        name.chars()
            .next()
            .is_some_and(|c| self.chantypes.contains(c))
    }

    /// Splits a STATUSMSG target such as `@#chan` into its prefix symbols
    /// and the channel. Symbols are only stripped when a channel follows,
    /// so a `+chan` channel stays whole.
    pub fn split_statusmsg<'a>(&self, target: &'a str) -> (&'a str, &'a str) {
        let run = target
            .char_indices()
            .find(|(_, c)| !self.statusmsg.contains(*c))
            .map_or(target.len(), |(i, _)| i);
        // Longest run of symbols that still leaves a channel name, so
        // `@+chan` on a server with `+` channels is `@` to `+chan`.
        (1..=run)
            .rev()
            .filter(|&end| target.is_char_boundary(end))
            .map(|end| target.split_at(end))
            .find(|(_, rest)| self.is_channel(rest))
            .unwrap_or(("", target))
    }

    /// Whether `mode` consumes a parameter when set (`adding`) or unset.
    /// Unknown modes are taken to have none, as most servers' user modes.
    pub fn takes_param(&self, mode: char, adding: bool) -> bool {
        if self.rank_of_mode(mode).is_some() {
            return true;
        }
        let [a, b, c, _] = &self.chanmodes;
        a.contains(mode) || b.contains(mode) || (adding && c.contains(mode))
    }

    /// Whether `mode` is a list mode (bans, exceptions, invites).
    pub fn is_list_mode(&self, mode: char) -> bool {
        self.chanmodes[0].contains(mode)
    }
}

/// `(qaohv)~&@%+` into pairs. A malformed value keeps the default.
fn parse_prefix(value: &str) -> Vec<(char, char)> {
    let Some((modes, symbols)) = value
        .strip_prefix('(')
        .and_then(|rest| rest.split_once(')'))
    else {
        return if value.is_empty() {
            Vec::new()
        } else {
            DEFAULT_PREFIX.to_vec()
        };
    };
    if modes.chars().count() != symbols.chars().count() {
        return DEFAULT_PREFIX.to_vec();
    }
    modes.chars().zip(symbols.chars()).collect()
}

/// 005 values escape bytes as `\xHH`.
fn unescape(value: &str) -> String {
    let mut out = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && bytes.get(i + 1) == Some(&b'x')
            && let Some(hex) = value.get(i + 2..i + 4)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 4;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(tokens: &[&str]) -> Isupport {
        let mut i = Isupport::default();
        i.apply(tokens);
        i
    }

    #[test]
    fn defaults_match_textual() {
        let i = Isupport::default();
        assert_eq!(i.prefix(), &[('o', '@'), ('v', '+')]);
        assert!(i.is_channel("#a") && i.is_channel("&a") && !i.is_channel("a"));
        assert_eq!(i.casemapping, CaseMapping::Rfc1459);
    }

    #[test]
    fn reads_an_ergo_005() {
        let i = parsed(&[
            "CASEMAPPING=ascii",
            "CHANMODES=Ibe,k,fl,CEMRUimnstu",
            "CHANTYPES=#",
            "NETWORK=ErgoTest",
            "NICKLEN=32",
            "PREFIX=(qaohv)~&@%+",
            "STATUSMSG=~&@%+",
            "WHOX",
        ]);
        assert_eq!(i.network.as_deref(), Some("ErgoTest"));
        assert_eq!(i.casemapping, CaseMapping::Ascii);
        assert_eq!(i.nicklen, Some(32));
        assert!(i.whox);
        assert_eq!(i.rank_of_mode('q'), Some(0));
        assert_eq!(i.rank_of_mode('v'), Some(4));
        assert_eq!(i.mode_of_symbol('%'), Some('h'));
        assert!(!i.is_channel("&local"));
    }

    #[test]
    fn mode_parameters_follow_chanmodes_groups() {
        let i = parsed(&["CHANMODES=beI,k,l,imnpst", "PREFIX=(ov)@+"]);
        assert!(i.takes_param('b', true) && i.takes_param('b', false));
        assert!(i.takes_param('k', true) && i.takes_param('k', false));
        assert!(i.takes_param('l', true) && !i.takes_param('l', false));
        assert!(!i.takes_param('m', true));
        assert!(i.takes_param('o', false));
        assert!(i.is_list_mode('b') && !i.is_list_mode('k'));
    }

    #[test]
    fn statusmsg_only_strips_before_a_channel() {
        let i = parsed(&["STATUSMSG=@+", "CHANTYPES=#+"]);
        assert_eq!(i.split_statusmsg("@#chan"), ("@", "#chan"));
        assert_eq!(i.split_statusmsg("@+#chan"), ("@+", "#chan"));
        assert_eq!(i.split_statusmsg("+modeless"), ("", "+modeless"));
        assert_eq!(i.split_statusmsg("#plain"), ("", "#plain"));
        assert_eq!(i.split_statusmsg("@"), ("", "@"));
        assert_eq!(i.split_statusmsg("@+modeless"), ("@", "+modeless"));
    }

    #[test]
    fn negation_and_escapes() {
        let mut i = parsed(&["NETWORK=Foo\\x20Net", "PREFIX=(qov)~@+"]);
        assert_eq!(i.network.as_deref(), Some("Foo Net"));
        i.apply(&["-PREFIX", "-NETWORK"]);
        assert_eq!(i.prefix(), &[('o', '@'), ('v', '+')]);
        assert_eq!(i.network, None);
    }

    #[test]
    fn malformed_prefix_keeps_the_default() {
        assert_eq!(
            parsed(&["PREFIX=(ov)@"]).prefix(),
            &[('o', '@'), ('v', '+')]
        );
        assert_eq!(parsed(&["PREFIX="]).prefix(), &[]);
    }
}
