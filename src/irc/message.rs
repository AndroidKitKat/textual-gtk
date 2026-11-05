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
    /// atoms count as one separator, as RFC 1459 allows. The command is
    /// uppercased, since IRC commands are case-insensitive.
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
            command: command.to_ascii_uppercase(),
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

fn escape_tag_value(value: &str, out: &mut String) {
    for c in value.chars() {
        match c {
            ';' => out.push_str("\\:"),
            ' ' => out.push_str("\\s"),
            '\\' => out.push_str("\\\\"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
}

/// Why a `Message` cannot be put on the wire as it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// CR, LF or NUL outside a tag value, which would end the line early and
    /// let the rest be read as another command.
    LineBreak,
    /// An atom that would split, vanish or change meaning when re-parsed.
    Malformed(&'static str),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::LineBreak => f.write_str("contains CR, LF or NUL"),
            WireError::Malformed(what) => write!(f, "malformed {what}"),
        }
    }
}

impl std::error::Error for WireError {}

fn check_atom(atom: &str, what: &'static str) -> Result<(), WireError> {
    if atom.contains(['\r', '\n', '\0']) {
        Err(WireError::LineBreak)
    } else if atom.is_empty() || atom.contains(' ') {
        Err(WireError::Malformed(what))
    } else {
        Ok(())
    }
}

impl Message {
    /// The line to send, CRLF included. Fails rather than emit a line that
    /// would parse back as anything other than this message.
    pub fn to_wire(&self) -> Result<String, WireError> {
        let mut out = String::with_capacity(64);
        if !self.tags.is_empty() {
            out.push('@');
            for (i, (key, value)) in self.tags.iter().enumerate() {
                check_atom(key, "tag key")?;
                if key.contains([';', '=']) {
                    return Err(WireError::Malformed("tag key"));
                }
                if value.contains('\0') {
                    return Err(WireError::LineBreak);
                }
                if i > 0 {
                    out.push(';');
                }
                out.push_str(key);
                if !value.is_empty() {
                    out.push('=');
                    escape_tag_value(value, &mut out);
                }
            }
            out.push(' ');
        }
        if let Some(source) = &self.source {
            check_atom(source, "source")?;
            out.push(':');
            out.push_str(source);
            out.push(' ');
        }
        check_atom(&self.command, "command")?;
        out.push_str(&self.command);
        if let Some((last, middle)) = self.params.split_last() {
            for param in middle {
                check_atom(param, "middle parameter")?;
                if param.starts_with(':') {
                    return Err(WireError::Malformed("middle parameter"));
                }
                out.push(' ');
                out.push_str(param);
            }
            if last.contains(['\r', '\n', '\0']) {
                return Err(WireError::LineBreak);
            }
            out.push(' ');
            if last.is_empty() || last.starts_with(':') || last.contains(' ') {
                out.push(':');
            }
            out.push_str(last);
        }
        out.push_str("\r\n");
        Ok(out)
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
