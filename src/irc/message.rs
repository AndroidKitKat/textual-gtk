use std::fmt;

/// One IRC protocol line, with IRCv3 message tags.
///
/// A tag sent without a value (`@foo`) is stored with an empty value, matching
/// the IRCv3 rule that the two forms are equivalent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Message {
    pub tags: Vec<(String, String)>,
    pub source: Option<String>,
    pub command: String,
    pub params: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    MissingCommand,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Empty => f.write_str("empty line"),
            ParseError::MissingCommand => f.write_str("line has no command"),
        }
    }
}

impl std::error::Error for ParseError {}

impl Message {
    pub fn new(command: &str, params: &[&str]) -> Self {
        Message {
            command: command.to_owned(),
            params: params.iter().map(|p| (*p).to_owned()).collect(),
            ..Default::default()
        }
    }

    /// Parses one line. A trailing CR/LF is ignored. Runs of spaces between
    /// atoms count as one separator, as RFC 1459 allows.
    pub fn parse(line: &str) -> Result<Self, ParseError> {
        let mut rest = line.trim_end_matches(['\r', '\n']).trim_start_matches(' ');
        if rest.is_empty() {
            return Err(ParseError::Empty);
        }

        let mut tags = Vec::new();
        if let Some(after) = rest.strip_prefix('@') {
            let (raw, tail) = split_atom(after);
            for item in raw.split(';').filter(|item| !item.is_empty()) {
                let (key, value) = item.split_once('=').unwrap_or((item, ""));
                let value = unescape_tag_value(value);
                match tags
                    .iter_mut()
                    .find(|(k, _): &&mut (String, String)| k == key)
                {
                    Some(existing) => existing.1 = value,
                    None => tags.push((key.to_owned(), value)),
                }
            }
            rest = tail;
        }

        let mut source = None;
        if let Some(after) = rest.strip_prefix(':') {
            let (raw, tail) = split_atom(after);
            source = Some(raw.to_owned());
            rest = tail;
        }

        let (command, mut rest) = split_atom(rest);
        if command.is_empty() {
            return Err(ParseError::MissingCommand);
        }

        let mut params = Vec::new();
        while !rest.is_empty() {
            if let Some(trailing) = rest.strip_prefix(':') {
                params.push(trailing.to_owned());
                break;
            }
            let (param, tail) = split_atom(rest);
            params.push(param.to_owned());
            rest = tail;
        }

        Ok(Message {
            tags,
            source,
            command: command.to_owned(),
            params,
        })
    }

    pub fn tag(&self, key: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn source(&self) -> Option<Source<'_>> {
        self.source.as_deref().map(Source::parse)
    }

    pub fn param(&self, index: usize) -> Option<&str> {
        self.params.get(index).map(String::as_str)
    }
}

/// Splits off one space-delimited atom and skips the spaces after it.
fn split_atom(input: &str) -> (&str, &str) {
    match input.split_once(' ') {
        Some((atom, tail)) => (atom, tail.trim_start_matches(' ')),
        None => (input, ""),
    }
}

fn unescape_tag_value(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(':') => out.push(';'),
            Some('s') => out.push(' '),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

fn escape_tag_value(value: &str, out: &mut fmt::Formatter<'_>) -> fmt::Result {
    for c in value.chars() {
        match c {
            ';' => out.write_str("\\:")?,
            ' ' => out.write_str("\\s")?,
            '\\' => out.write_str("\\\\")?,
            '\r' => out.write_str("\\r")?,
            '\n' => out.write_str("\\n")?,
            c => write!(out, "{c}")?,
        }
    }
    Ok(())
}

/// Wire form without the CRLF terminator.
impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.tags.is_empty() {
            f.write_str("@")?;
            for (i, (key, value)) in self.tags.iter().enumerate() {
                if i > 0 {
                    f.write_str(";")?;
                }
                f.write_str(key)?;
                if !value.is_empty() {
                    f.write_str("=")?;
                    escape_tag_value(value, f)?;
                }
            }
            f.write_str(" ")?;
        }
        if let Some(source) = &self.source {
            write!(f, ":{source} ")?;
        }
        f.write_str(&self.command)?;
        if let Some((last, middle)) = self.params.split_last() {
            for param in middle {
                write!(f, " {param}")?;
            }
            if last.is_empty() || last.starts_with(':') || last.contains(' ') {
                write!(f, " :{last}")?;
            } else {
                write!(f, " {last}")?;
            }
        }
        Ok(())
    }
}

/// A message source split into `nick!user@host`. A server name lands in `nick`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source<'a> {
    pub nick: &'a str,
    pub user: Option<&'a str>,
    pub host: Option<&'a str>,
}

impl<'a> Source<'a> {
    pub fn parse(raw: &'a str) -> Self {
        let (rest, host) = match raw.split_once('@') {
            Some((rest, host)) => (rest, Some(host)),
            None => (raw, None),
        };
        let (nick, user) = match rest.split_once('!') {
            Some((nick, user)) => (nick, Some(user)),
            None => (rest, None),
        };
        Source { nick, user, host }
    }
}
