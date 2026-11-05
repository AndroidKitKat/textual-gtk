//! The subset of GRMustache 7 that Textual's styles use: spec Mustache, the
//! `isEmpty(x)` filter, and an else clause (`{{^x}}` inside an open `{{#x}}`).
//! Anything else is rejected at parse time so a user style fails loudly.

use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Str(String),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}

impl Value {
    /// GRMustache truthiness: false, the empty string and the empty list are
    /// falsy, like a missing key.
    fn is_truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Str(s) => !s.is_empty(),
            Value::List(items) => !items.is_empty(),
            Value::Map(_) => true,
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_owned())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<Vec<String>> for Value {
    fn from(items: Vec<String>) -> Self {
        Value::List(items.into_iter().map(Value::Str).collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for TemplateError {}

/// A tag's subject: a key, `.`, or the `isEmpty` filter applied to a key.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expr {
    Implicit,
    Key(String),
    IsEmpty(String),
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Text(String),
    Var {
        expr: Expr,
        escape: bool,
    },
    Section {
        expr: Expr,
        inverted: bool,
        body: Vec<Node>,
        otherwise: Vec<Node>,
    },
    Partial(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    nodes: Vec<Node>,
}

impl Template {
    pub fn parse(source: &str) -> Result<Self, TemplateError> {
        let tokens = tokenize(source)?;
        let mut stack: Vec<Frame> = vec![Frame::root()];
        for token in tokens {
            match token.kind {
                TokenKind::Text(text) => stack.last_mut().unwrap().push(Node::Text(text)),
                TokenKind::Comment => {}
                TokenKind::Var { expr, escape } => {
                    stack.last_mut().unwrap().push(Node::Var { expr, escape })
                }
                TokenKind::Partial(name) => stack.last_mut().unwrap().push(Node::Partial(name)),
                TokenKind::Open { expr, inverted } => {
                    let top = stack.last_mut().unwrap();
                    let is_else = inverted
                        && !top.inverted
                        && !top.in_else
                        && top.expr.as_ref() == Some(&expr);
                    if is_else {
                        top.in_else = true;
                    } else if stack.len() > MAX_NESTING {
                        return Err(error(token.line, "sections nested too deeply".into()));
                    } else {
                        stack.push(Frame::section(expr, inverted, token.line));
                    }
                }
                TokenKind::Close(expr) => {
                    if stack.len() == 1 {
                        return Err(error(token.line, format!("unexpected close of {expr:?}")));
                    }
                    let frame = stack.pop().unwrap();
                    let open = frame.expr.clone().unwrap();
                    if open != expr {
                        return Err(error(
                            token.line,
                            format!("{expr:?} closes {open:?} opened on line {}", frame.line),
                        ));
                    }
                    stack.last_mut().unwrap().push(Node::Section {
                        expr: open,
                        inverted: frame.inverted,
                        body: frame.body,
                        otherwise: frame.otherwise,
                    });
                }
            }
        }
        if stack.len() > 1 {
            let frame = stack.pop().unwrap();
            return Err(error(
                frame.line,
                format!("section {:?} is never closed", frame.expr.unwrap()),
            ));
        }
        Ok(Template {
            nodes: stack.pop().unwrap().body,
        })
    }

    /// Renders against `context`, resolving `{{> name}}` through `partial`.
    pub fn render<'t>(
        &self,
        context: &Value,
        partial: &dyn Fn(&str) -> Option<&'t Template>,
    ) -> String {
        let mut out = String::new();
        let mut stack = vec![context];
        render_nodes(&self.nodes, &mut stack, partial, &mut out, &mut Vec::new());
        out
    }
}

struct Frame {
    expr: Option<Expr>,
    inverted: bool,
    line: usize,
    in_else: bool,
    body: Vec<Node>,
    otherwise: Vec<Node>,
}

impl Frame {
    fn root() -> Self {
        Frame {
            expr: None,
            inverted: false,
            line: 0,
            in_else: false,
            body: Vec::new(),
            otherwise: Vec::new(),
        }
    }

    fn section(expr: Expr, inverted: bool, line: usize) -> Self {
        Frame {
            expr: Some(expr),
            inverted,
            line,
            ..Frame::root()
        }
    }

    fn push(&mut self, node: Node) {
        if self.in_else {
            self.otherwise.push(node)
        } else {
            self.body.push(node)
        }
    }
}

fn error(line: usize, message: String) -> TemplateError {
    TemplateError { line, message }
}

/// Deeper nesting is refused so rendering cannot overflow the stack.
const MAX_NESTING: usize = 128;

fn render_nodes<'v, 't>(
    nodes: &[Node],
    stack: &mut Vec<&'v Value>,
    partial: &dyn Fn(&str) -> Option<&'t Template>,
    out: &mut String,
    active_partials: &mut Vec<String>,
) where
    't: 'v,
{
    for node in nodes {
        match node {
            Node::Text(text) => out.push_str(text),
            Node::Var { expr, escape } => {
                if let Some(Value::Str(s)) = lookup(stack, expr) {
                    if *escape {
                        escape_html(s, out)
                    } else {
                        out.push_str(s)
                    }
                } else if let Some(Value::Bool(b)) = lookup(stack, expr) {
                    out.push_str(if *b { "1" } else { "0" });
                }
            }
            Node::Section {
                expr,
                inverted,
                body,
                otherwise,
            } => {
                let value = lookup(stack, expr);
                let truthy = value.is_some_and(Value::is_truthy);
                if *inverted {
                    let branch = if truthy { otherwise } else { body };
                    render_nodes(branch, stack, partial, out, active_partials);
                } else if !truthy {
                    render_nodes(otherwise, stack, partial, out, active_partials);
                } else {
                    match value.unwrap() {
                        Value::List(items) => {
                            for item in items {
                                stack.push(item);
                                render_nodes(body, stack, partial, out, active_partials);
                                stack.pop();
                            }
                        }
                        other => {
                            stack.push(other);
                            render_nodes(body, stack, partial, out, active_partials);
                            stack.pop();
                        }
                    }
                }
            }
            Node::Partial(name) => {
                if !active_partials.contains(name)
                    && let Some(template) = partial(name)
                {
                    active_partials.push(name.clone());
                    render_nodes(&template.nodes, stack, partial, out, active_partials);
                    active_partials.pop();
                }
            }
        }
    }
}

/// `isEmpty(x)` yields true for a missing, false or empty `x`, so it is
/// returned as a borrowed constant rather than a lookup result.
fn lookup<'v>(stack: &[&'v Value], expr: &Expr) -> Option<&'v Value> {
    static TRUE: Value = Value::Bool(true);
    static FALSE: Value = Value::Bool(false);
    match expr {
        Expr::Implicit => stack.last().copied(),
        Expr::Key(key) => find(stack, key),
        Expr::IsEmpty(key) => {
            let empty = !find(stack, key).is_some_and(Value::is_truthy);
            Some(if empty { &TRUE } else { &FALSE })
        }
    }
}

fn find<'v>(stack: &[&'v Value], key: &str) -> Option<&'v Value> {
    stack.iter().rev().find_map(|value| match value {
        Value::Map(map) => map.get(key),
        _ => None,
    })
}

/// GRMustache's escape set.
fn escape_html(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
}

struct Token {
    kind: TokenKind,
    line: usize,
}

enum TokenKind {
    Text(String),
    Comment,
    Var { expr: Expr, escape: bool },
    Open { expr: Expr, inverted: bool },
    Close(Expr),
    Partial(String),
}

impl TokenKind {
    /// Tags that leave no output; on a line of their own the whole line goes.
    fn is_standalone_kind(&self) -> bool {
        matches!(
            self,
            TokenKind::Comment
                | TokenKind::Open { .. }
                | TokenKind::Close(_)
                | TokenKind::Partial(_)
        )
    }
}

fn tokenize(source: &str) -> Result<Vec<Token>, TemplateError> {
    let mut tokens = Vec::new();
    let mut rest = source;
    let mut line = 1;
    while let Some(start) = rest.find("{{") {
        let (text, after) = rest.split_at(start);
        if !text.is_empty() {
            tokens.push(Token {
                kind: TokenKind::Text(text.to_owned()),
                line,
            });
        }
        line += text.matches('\n').count();
        let (kind, consumed) = parse_tag(after, line)?;
        line += after[..consumed].matches('\n').count();
        tokens.push(Token { kind, line });
        rest = &after[consumed..];
    }
    if !rest.is_empty() {
        tokens.push(Token {
            kind: TokenKind::Text(rest.to_owned()),
            line,
        });
    }
    strip_standalone_lines(&mut tokens);
    Ok(tokens)
}

fn parse_tag(input: &str, line: usize) -> Result<(TokenKind, usize), TemplateError> {
    if let Some(body) = input.strip_prefix("{{{") {
        let end = body
            .find("}}}")
            .ok_or_else(|| error(line, "unterminated {{{ tag".into()))?;
        let expr = parse_expr(body[..end].trim(), line)?;
        return Ok((
            TokenKind::Var {
                expr,
                escape: false,
            },
            3 + end + 3,
        ));
    }
    let body = &input[2..];
    let end = body
        .find("}}")
        .ok_or_else(|| error(line, "unterminated {{ tag".into()))?;
    let inner = &body[..end];
    let consumed = 2 + end + 2;
    let (sigil, name) = match inner.trim_start().chars().next() {
        Some(c @ ('#' | '^' | '/' | '>' | '!' | '&' | '=' | '<' | '$' | '%')) => {
            (Some(c), inner.trim_start()[1..].trim())
        }
        _ => (None, inner.trim()),
    };
    let kind = match sigil {
        None => TokenKind::Var {
            expr: parse_expr(name, line)?,
            escape: true,
        },
        Some('&') => TokenKind::Var {
            expr: parse_expr(name, line)?,
            escape: false,
        },
        Some('#') => TokenKind::Open {
            expr: parse_expr(name, line)?,
            inverted: false,
        },
        Some('^') => TokenKind::Open {
            expr: parse_expr(name, line)?,
            inverted: true,
        },
        Some('/') => TokenKind::Close(parse_expr(name, line)?),
        Some('>') => TokenKind::Partial(name.to_owned()),
        Some('!') => TokenKind::Comment,
        Some(other) => {
            return Err(error(
                line,
                format!("unsupported tag {{{{{other}…}}}} (not used by Textual styles)"),
            ));
        }
    };
    Ok((kind, consumed))
}

fn parse_expr(text: &str, line: usize) -> Result<Expr, TemplateError> {
    if text == "." {
        return Ok(Expr::Implicit);
    }
    if let Some(arg) = text
        .strip_prefix("isEmpty(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return Ok(Expr::IsEmpty(check_key(arg.trim(), line)?.to_owned()));
    }
    Ok(Expr::Key(check_key(text, line)?.to_owned()))
}

fn check_key(key: &str, line: usize) -> Result<&str, TemplateError> {
    let valid = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if valid {
        Ok(key)
    } else {
        Err(error(line, format!("unsupported tag expression {key:?}")))
    }
}

/// Spec rule: a section, comment or partial tag alone on its line (only
/// whitespace around it) removes that whitespace and the line's newline.
fn strip_standalone_lines(tokens: &mut [Token]) {
    let standalone: Vec<bool> = (0..tokens.len())
        .map(|i| is_standalone(tokens, i))
        .collect();
    for (i, _) in standalone.iter().enumerate().filter(|(_, s)| **s) {
        if i > 0
            && let TokenKind::Text(t) = &mut tokens[i - 1].kind
        {
            let keep = t.rfind('\n').map_or(0, |n| n + 1);
            t.truncate(keep);
        }
        if let Some(Token {
            kind: TokenKind::Text(t),
            ..
        }) = tokens.get_mut(i + 1)
        {
            let drop = t.find('\n').map_or(t.len(), |n| n + 1);
            t.replace_range(..drop, "");
        }
    }
}

fn is_standalone(tokens: &[Token], i: usize) -> bool {
    let blank = |s: &str| s.chars().all(|c| c == ' ' || c == '\t' || c == '\r');
    if !tokens[i].kind.is_standalone_kind() {
        return false;
    }
    let starts_line = match i.checked_sub(1).map(|j| &tokens[j].kind) {
        None => true,
        Some(TokenKind::Text(t)) => {
            blank(t.rsplit('\n').next().unwrap()) && (t.contains('\n') || i == 1)
        }
        Some(_) => false,
    };
    let ends_line = match tokens.get(i + 1).map(|t| &t.kind) {
        None => true,
        Some(TokenKind::Text(t)) => {
            blank(t.split('\n').next().unwrap()) && (t.contains('\n') || i + 2 == tokens.len())
        }
        Some(_) => false,
    };
    starts_line && ends_line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, Value)]) -> Value {
        Value::Map(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        )
    }

    fn render(source: &str, context: &Value) -> String {
        Template::parse(source).unwrap().render(context, &|_| None)
    }

    #[test]
    fn escapes_variables_but_not_triples() {
        let ctx = map(&[("x", "<a href='b'>&\"".into())]);
        assert_eq!(
            render("{{x}}|{{{x}}}|{{&x}}", &ctx),
            "&lt;a href=&apos;b&apos;&gt;&amp;&quot;|<a href='b'>&\"|<a href='b'>&\""
        );
    }

    #[test]
    fn else_clause_picks_the_matching_branch() {
        let source = "{{#c}}yes {{c}}{{^c}}no{{/c}}";
        assert_eq!(render(source, &map(&[("c", "1".into())])), "yes 1");
        assert_eq!(render(source, &map(&[("c", false.into())])), "no");
        assert_eq!(render(source, &map(&[])), "no");
    }

    #[test]
    fn is_empty_filter_tests_for_a_blank_value() {
        let source = "{{^ isEmpty(t) }}[{{t}}]{{/ isEmpty(t) }}";
        assert_eq!(render(source, &map(&[("t", "12:00".into())])), "[12:00]");
        assert_eq!(render(source, &map(&[("t", "".into())])), "");
        assert_eq!(render(source, &map(&[])), "");
    }

    #[test]
    fn lists_iterate_with_the_parent_context_visible() {
        let ctx = map(&[
            ("files", vec!["a.css".to_owned(), "b.css".to_owned()].into()),
            ("token", "42".into()),
        ]);
        assert_eq!(
            render("{{#files}}<{{.}}?{{token}}>{{/files}}", &ctx),
            "<a.css?42><b.css?42>"
        );
    }

    #[test]
    fn empty_string_and_empty_list_are_falsy() {
        let ctx = map(&[("s", "".into()), ("l", Value::List(vec![]))]);
        assert_eq!(render("{{#s}}x{{/s}}{{^s}}S{{/s}}", &ctx), "S");
        assert_eq!(render("{{#l}}x{{/l}}{{^l}}L{{/l}}", &ctx), "L");
    }

    #[test]
    fn standalone_tags_remove_their_line() {
        let source = "<a>\n    {{#x}}\n    <b>\n    {{/x}}\n</a>";
        assert_eq!(
            render(source, &map(&[("x", true.into())])),
            "<a>\n    <b>\n</a>"
        );
    }

    #[test]
    fn consecutive_standalone_tags_all_remove_their_lines() {
        let source = "<a>\n{{#x}}\n{{#y}}\n<b>\n{{/y}}\n{{/x}}\n</a>";
        let ctx = map(&[("x", true.into()), ("y", true.into())]);
        assert_eq!(render(source, &ctx), "<a>\n<b>\n</a>");
    }

    #[test]
    fn partials_resolve_through_the_callback() {
        let inner = Template::parse("[{{n}}]").unwrap();
        let outer = Template::parse("a{{> inner}}b{{> missing}}c").unwrap();
        let got = outer.render(&map(&[("n", "1".into())]), &|name| {
            (name == "inner").then_some(&inner)
        });
        assert_eq!(got, "a[1]bc");
    }

    #[test]
    fn a_partial_that_includes_itself_renders_once() {
        let looping = Template::parse("x{{>p}}{{>p}}{{>p}}").unwrap();
        let got = looping.render(&map(&[]), &|_| Some(&looping));
        assert_eq!(got, "xxxx");
    }

    #[test]
    fn refuses_nesting_deep_enough_to_overflow() {
        let deep = format!("{}x{}", "{{#a}}".repeat(1000), "{{/a}}".repeat(1000));
        assert!(Template::parse(&deep).is_err());
        let fine = format!("{}x{}", "{{#a}}".repeat(100), "{{/a}}".repeat(100));
        assert!(Template::parse(&fine).is_ok());
    }

    #[test]
    fn rejects_constructs_textual_styles_never_use() {
        for source in [
            "{{#a}}",
            "{{/a}}",
            "{{#a}}{{/b}}",
            "{{=<% %>=}}",
            "{{a.b}}",
            "{{< layout}}",
            "{{x",
        ] {
            assert!(Template::parse(source).is_err(), "{source:?} should fail");
        }
    }
}
