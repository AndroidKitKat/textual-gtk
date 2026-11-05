//! The transcript runner: drives one `Network` through a `.irc` script in
//! virtual time.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use textual_core::irc::Message;
use textual_core::irc::caps::WANTED;
use textual_core::irc::casemap::CaseMapping;
use textual_core::model::network::{
    Address, CloseReason, ConnId, Effect, Ended, Input, Link, Network, Notice, ServerConfig,
    ServerId, Tls, TransportEvent,
};
use textual_core::model::registration::Auth;
use textual_core::secret::Secret;

use super::setup;

pub struct Driver {
    net: Network,
    now: Instant,
    latest: Option<ConnId>,
    sent: VecDeque<String>,
    notices: VecDeque<Notice>,
}

impl Driver {
    pub fn new(config: ServerConfig) -> Self {
        Driver {
            net: Network::new(config),
            now: Instant::now(),
            latest: None,
            sent: VecDeque::new(),
            notices: VecDeque::new(),
        }
    }

    pub fn input(&mut self, input: Input) {
        let mut out = Vec::new();
        self.net.update(input, self.now, &mut out);
        for effect in out {
            let item = match effect {
                Effect::Dial { conn, address, tls } => {
                    self.latest = Some(conn);
                    let tls = match tls {
                        Tls::Off => "plain",
                        Tls::On { verify: true } => "tls",
                        Tls::On { verify: false } => "tls-unverified",
                    };
                    format!("dial {conn} {address} {tls}")
                }
                Effect::Send { conn, line } => {
                    let text = line.as_str().strip_suffix("\r\n").expect("CRLF");
                    if Some(conn) == self.latest {
                        text.to_owned()
                    } else {
                        format!("{conn} {text}")
                    }
                }
                Effect::Hangup { conn } => format!("hangup {conn}"),
                Effect::Notice(notice) => {
                    self.notices.push_back(notice);
                    continue;
                }
            };
            self.sent.push_back(item);
        }
    }

    pub fn transport(&mut self, conn: ConnId, event: TransportEvent) {
        self.input(Input::Transport { conn, event });
    }

    pub fn advance(&mut self, by: Duration) {
        self.now += by;
        self.input(Input::Wake);
    }

    pub fn network(&self) -> &Network {
        &self.net
    }
}

/// Runs one transcript and returns every mismatch, each with its line
/// number. The DSL:
///
/// - `= key value` configures the server before the first exchange: `nick`,
///   `alternates`, `user`, `realname`, `auth none|sasl-plain|nickserv`,
///   `server-password`, `account-password`, `caps` (replaces the wanted
///   list), `id` (a number, seeds the backoff jitter), `addresses`
///   (`host:port` list), `tls off|on|unverified`, `auto-reconnect on|off`.
/// - `> item`: the next thing the client did. A line sent on the latest
///   connection is written as is; on an older one it starts with `#n`.
///   `dial #n host:port plain|tls|tls-unverified` and `hangup #n`.
/// - `< line`: a server line on the latest connection.
/// - `! connect`, `! disconnect [reason]`: user input. `! opened`,
///   `! closed eof|timeout|error <text>|bad-certificate <hex>`: a transport
///   event on the latest connection; `! #n <event>` sends it on connection n,
///   where `line <raw>` is also an event.
/// - `@ +5s`, `@ +4.613s`, `@ +250ms`: virtual time passes. `@ wake`: time
///   passes to the network's next wake.
/// - `? notice text`: the next console notice, as displayed.
/// - Link queries, which are also the end states (every transcript must end
///   with one, so a truncated one cannot pass): `? registering`,
///   `? connecting`, `? quitting`, `? registered nick [account]`,
///   `? waiting <duration>` (until the retry), `? offline [why]`, and
///   `? failed no-usable-nick|error <text>` (the last link ended so).
/// - Other queries: `? next-wake <duration>|none`, `? failures n`,
///   `? desired online|offline`, `? nick x`, `? hostmask user@host`,
///   `? casemap ascii|rfc1459|rfc1459-strict`, `? network name`.
/// - Without a `!` or `@` before the first `>` or `<`, the script starts
///   already connected: the connect, its notice and dial, and `opened` run
///   unchecked.
/// - `#` at the start of a line is a comment; blank lines are skipped.
pub fn run_transcript(source: &str) -> Result<(), String> {
    let mut config = ServerConfig {
        id: ServerId(1),
        addresses: vec![address("irc.test:6667")],
        tls: Tls::Off,
        auto_reconnect: true,
        setup: setup("textual"),
        wanted_caps: WANTED.iter().map(|c| (*c).to_owned()).collect(),
    };
    let mut driver: Option<Driver> = None;
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
            if driver.is_some() {
                return Err(format!("{n}: configuration after the first exchange"));
            }
            configure(&mut config, rest).map_err(|e| format!("{n}: {e}"))?;
            continue;
        }

        let t = driver.get_or_insert_with(|| {
            let mut t = Driver::new(config.clone());
            if !matches!(kind, "!" | "@") {
                t.input(Input::Connect);
                t.transport(t.latest.expect("dialed"), TransportEvent::Opened);
                t.sent.retain(|s| !s.starts_with("dial "));
                t.notices.retain(|n| !matches!(n, Notice::Connecting(_)));
            }
            t
        });
        ends_in_state = kind == "?" && is_end_state(rest);
        if matches!(kind, "<" | "!" | "@") {
            while let Some(extra) = t.sent.pop_front() {
                writeln!(errors, "{n}: unexpected sent {extra:?}").unwrap();
            }
        }
        let result = match kind {
            ">" => {
                match t.sent.pop_front() {
                    Some(got) if got == rest => {}
                    Some(got) => {
                        writeln!(errors, "{n}: expected sent {rest:?}, got {got:?}").unwrap();
                    }
                    None => {
                        writeln!(errors, "{n}: expected sent {rest:?}, nothing was sent").unwrap();
                    }
                }
                Ok(())
            }
            "<" => match t.latest {
                Some(conn) => parse_line(rest).map(|m| t.transport(conn, TransportEvent::Line(m))),
                None => Err("a server line before any dial".to_owned()),
            },
            "!" => event(t, rest),
            "@" => time(t, rest),
            "?" => {
                if let Some(got) = check_query(t, rest) {
                    writeln!(errors, "{n}: expected {rest:?}, got {got}").unwrap();
                }
                Ok(())
            }
            _ => Err(format!("unknown line kind {kind:?}")),
        };
        result.map_err(|e| format!("{n}: {e}"))?;
    }

    if let Some(t) = &mut driver {
        while let Some(extra) = t.sent.pop_front() {
            writeln!(errors, "end: unexpected sent {extra:?}").unwrap();
        }
        while let Some(extra) = t.notices.pop_front() {
            writeln!(errors, "end: unchecked notice {:?}", extra.to_string()).unwrap();
        }
    }
    if !ends_in_state {
        errors.push_str("end: no end state (a link query such as `? registered`)\n");
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn configure(config: &mut ServerConfig, rest: &str) -> Result<(), String> {
    let (key, value) = rest.split_once(' ').unwrap_or((rest, ""));
    let s = &mut config.setup;
    match key {
        "nick" => value.clone_into(&mut s.nick),
        "alternates" => s.alternates = value.split_whitespace().map(str::to_owned).collect(),
        "user" => value.clone_into(&mut s.username),
        "realname" => value.clone_into(&mut s.realname),
        "auth" => {
            s.auth = match value {
                "none" => Auth::None,
                "sasl-plain" => Auth::SaslPlain,
                "nickserv" => Auth::NickServ,
                _ => return Err(format!("unknown auth {value:?}")),
            };
        }
        "server-password" => s.server_password = Some(Secret::new(value)),
        "account-password" => s.account_password = Some(Secret::new(value)),
        "caps" => config.wanted_caps = value.split_whitespace().map(str::to_owned).collect(),
        "id" => config.id = ServerId(value.parse().map_err(|_| format!("bad id {value:?}"))?),
        "addresses" => config.addresses = value.split_whitespace().map(address).collect(),
        "tls" => {
            config.tls = match value {
                "off" => Tls::Off,
                "on" => Tls::On { verify: true },
                "unverified" => Tls::On { verify: false },
                _ => return Err(format!("unknown tls {value:?}")),
            };
        }
        "auto-reconnect" => config.auto_reconnect = value == "on",
        _ => return Err(format!("unknown setting {key:?}")),
    }
    Ok(())
}

fn address(text: &str) -> Address {
    let (host, port) = text.rsplit_once(':').expect("host:port");
    Address {
        host: host.to_owned(),
        port: port.parse().expect("a port number"),
    }
}

fn parse_line(raw: &str) -> Result<Message, String> {
    Message::parse(raw).map_err(|e| format!("bad line {raw:?}: {e}"))
}

fn event(t: &mut Driver, rest: &str) -> Result<(), String> {
    let (conn, rest) = match rest.strip_prefix('#') {
        Some(tail) => {
            let (n, tail) = tail.split_once(' ').ok_or("an event after the conn")?;
            let n = n.parse().map_err(|_| format!("bad conn {n:?}"))?;
            (Some(ConnId::new(n)), tail)
        }
        None => (t.latest, rest),
    };
    let (what, arg) = rest.split_once(' ').unwrap_or((rest, ""));
    let event = match what {
        "connect" => {
            t.input(Input::Connect);
            return Ok(());
        }
        "disconnect" => {
            let reason = (!arg.is_empty()).then(|| arg.to_owned());
            t.input(Input::Disconnect { reason });
            return Ok(());
        }
        "opened" => TransportEvent::Opened,
        "line" => TransportEvent::Line(parse_line(arg)?),
        "closed" => TransportEvent::Closed(close_reason(arg)?),
        _ => return Err(format!("unknown event {what:?}")),
    };
    let conn = conn.ok_or("a transport event before any dial")?;
    t.transport(conn, event);
    Ok(())
}

fn close_reason(arg: &str) -> Result<CloseReason, String> {
    let (what, text) = arg.split_once(' ').unwrap_or((arg, ""));
    Ok(match what {
        "eof" => CloseReason::Eof,
        "timeout" => CloseReason::ConnectTimeout,
        "error" => CloseReason::Error(text.to_owned()),
        "bad-certificate" => {
            let bytes: Vec<u8> = (0..text.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(text.get(i..i + 2).unwrap_or("zz"), 16))
                .collect::<Result<_, _>>()
                .map_err(|_| format!("bad fingerprint {text:?}"))?;
            let sha256 = bytes.try_into().map_err(|_| "a fingerprint is 32 bytes")?;
            CloseReason::BadCertificate { sha256 }
        }
        _ => return Err(format!("unknown close reason {what:?}")),
    })
}

fn time(t: &mut Driver, rest: &str) -> Result<(), String> {
    if rest == "wake" {
        let at = t
            .net
            .next_wake()
            .ok_or("`@ wake` with nothing to wake for")?;
        let by = at.saturating_duration_since(t.now);
        t.advance(by);
        return Ok(());
    }
    let by = rest
        .strip_prefix('+')
        .and_then(duration)
        .ok_or_else(|| format!("bad time {rest:?}, expected `+5s` or `wake`"))?;
    t.advance(by);
    Ok(())
}

/// `5s`, `4.613s` or `250ms`, to the millisecond.
fn duration(text: &str) -> Option<Duration> {
    if let Some(ms) = text.strip_suffix("ms") {
        return ms.parse().ok().map(Duration::from_millis);
    }
    let secs = text.strip_suffix('s')?;
    let (whole, frac) = secs.split_once('.').unwrap_or((secs, ""));
    if frac.len() > 3 {
        return None;
    }
    let whole: u64 = whole.parse().ok()?;
    let frac: u64 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<3}").parse().ok()?
    };
    Some(Duration::from_millis(whole * 1000 + frac))
}

fn show(d: Duration) -> String {
    let ms = d.as_millis();
    if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{}.{:03}s", ms / 1000, ms % 1000)
            .trim_end_matches('0')
            .to_owned()
    }
}

fn is_end_state(query: &str) -> bool {
    let what = query.split(' ').next().unwrap_or("");
    matches!(
        what,
        "registered" | "failed" | "registering" | "connecting" | "quitting" | "waiting" | "offline"
    )
}

fn ended_tag(why: &Ended) -> &'static str {
    match why {
        Ended::UserQuit => "user-quit",
        Ended::Closed(CloseReason::BadCertificate { .. }) => "bad-certificate",
        Ended::Closed(_) => "closed",
        Ended::ServerError(_) => "server-error",
        Ended::Banned(_) => "banned",
        Ended::RegistrationTimedOut => "registration-timed-out",
        Ended::PingTimeout => "ping-timeout",
        Ended::NoUsableNick { .. } => "no-usable-nick",
        Ended::BadSetup(_) => "bad-setup",
    }
}

fn last_ended(link: &Link) -> Option<&Ended> {
    match link {
        Link::Offline { last } => last.as_ref(),
        Link::Waiting { last, .. } => Some(last),
        _ => None,
    }
}

/// `None` when the query holds, else what was found instead.
fn check_query(t: &mut Driver, query: &str) -> Option<String> {
    let (what, arg) = query.split_once(' ').unwrap_or((query, ""));
    let net = &t.net;
    let link = net.link();
    let session = net.session();
    let holds = match what {
        "notice" => {
            return match t.notices.pop_front() {
                Some(notice) if notice.to_string() == arg => None,
                Some(notice) => Some(format!("notice {:?}", notice.to_string())),
                None => Some("no notice".to_owned()),
            };
        }
        "registered" => {
            let mut words = arg.split(' ');
            let nick = words.next().unwrap_or("");
            let account = words.next();
            matches!(link, Link::Online { .. })
                && session.is_some_and(|s| s.nick() == nick && s.account() == account)
        }
        "registering" => matches!(link, Link::Registering { .. }),
        "connecting" => matches!(link, Link::Connecting { .. }),
        "quitting" => matches!(link, Link::Quitting { .. }),
        "waiting" => match (link, duration(arg)) {
            (Link::Waiting { until, .. }, Some(d)) => *until == t.now + d,
            _ => false,
        },
        "offline" => match link {
            Link::Offline { last: None } => arg.is_empty(),
            Link::Offline { last: Some(why) } => arg.is_empty() || ended_tag(why) == arg,
            _ => false,
        },
        "failed" => match (arg.split_once(' ').unwrap_or((arg, "")), last_ended(link)) {
            (("no-usable-nick", _), Some(Ended::NoUsableNick { .. })) => true,
            (("error", text), Some(Ended::ServerError(got))) => got == text,
            _ => false,
        },
        "next-wake" => {
            let got = net.next_wake().map(|at| at - t.now);
            let want = if arg == "none" { None } else { duration(arg) };
            if got == want {
                return None;
            }
            return Some(got.map_or("none".to_owned(), show));
        }
        "failures" => net.failures().to_string() == arg,
        "desired" => format!("{:?}", net.desired()).eq_ignore_ascii_case(arg),
        "nick" => session.is_some_and(|s| s.nick() == arg),
        "hostmask" => session.is_some_and(|s| s.hostmask() == Some(arg)),
        "casemap" => session.is_some_and(|s| Some(s.casemap()) == CaseMapping::from_token(arg)),
        "network" => session.is_some_and(|s| s.isupport().network.as_deref() == Some(arg)),
        _ => return Some(format!("unknown query {what:?}")),
    };
    if holds {
        return None;
    }
    Some(match link {
        Link::Waiting { until, last } => {
            format!("waiting {} after {last:?}", show(*until - t.now))
        }
        Link::Online { session, .. } => format!(
            "online as {} ({:?}, {:?}), failures {}",
            session.nick(),
            session.hostmask(),
            session.isupport().casemapping,
            net.failures()
        ),
        other => format!("{other:?}, failures {}", net.failures()),
    })
}
