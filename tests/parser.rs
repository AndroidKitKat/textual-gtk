use textual_gtk::irc::{Message, Source, WireError};
use yaml_rust2::{Yaml, YamlLoader};

fn load(name: &str) -> Vec<Yaml> {
    let path = format!(
        "{}/tests/data/parser-tests/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap();
    let doc = YamlLoader::load_from_str(&text).unwrap().remove(0);
    doc["tests"].as_vec().unwrap().clone()
}

fn str_of(y: &Yaml) -> Option<String> {
    match y {
        Yaml::String(s) => Some(s.clone()),
        Yaml::Integer(i) => Some(i.to_string()),
        Yaml::Real(r) => Some(r.clone()),
        Yaml::Null | Yaml::BadValue => None,
        other => panic!("unexpected scalar {other:?}"),
    }
}

fn atoms_to_message(atoms: &Yaml) -> Message {
    let mut tags: Vec<(String, String)> = atoms["tags"]
        .as_hash()
        .map(|h| {
            h.iter()
                .map(|(k, v)| (str_of(k).unwrap(), str_of(v).unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default();
    tags.sort();
    Message {
        tags,
        source: str_of(&atoms["source"]),
        command: str_of(&atoms["verb"]).unwrap(),
        params: atoms["params"]
            .as_vec()
            .map(|v| v.iter().map(|p| str_of(p).unwrap()).collect())
            .unwrap_or_default(),
    }
}

#[test]
fn splits_every_ircdocs_vector() {
    let cases = load("msg-split.yaml");
    assert!(cases.len() > 30, "loaded {} cases", cases.len());
    for case in cases {
        let input = case["input"].as_str().unwrap();
        let mut expected = atoms_to_message(&case["atoms"]);
        expected.command.make_ascii_uppercase();
        let mut got = Message::parse(input).unwrap_or_else(|e| panic!("{input:?}: {e}"));
        got.tags.sort();
        assert_eq!(got, expected, "input {input:?}");
    }
}

#[test]
fn joins_every_ircdocs_vector() {
    for case in load("msg-join.yaml") {
        let message = atoms_to_message(&case["atoms"]);
        let matches: Vec<String> = case["matches"]
            .as_vec()
            .unwrap()
            .iter()
            .map(|m| str_of(m).unwrap())
            .collect();
        let wire = message.to_wire().unwrap();
        let wire = wire.strip_suffix("\r\n").unwrap().to_owned();
        assert!(
            matches.contains(&wire),
            "{:?}: produced {wire:?}, expected one of {matches:?}",
            case["desc"].as_str()
        );
    }
}

#[test]
fn splits_every_ircdocs_userhost() {
    for case in load("userhost-split.yaml") {
        let raw = case["source"].as_str().unwrap();
        let atoms = &case["atoms"];
        let got = Source::parse(raw);
        let field = |key: &str| str_of(&atoms[key]).unwrap_or_default();
        assert_eq!(got.nick, field("nick"), "nick of {raw:?}");
        assert_eq!(got.user.unwrap_or(""), field("user"), "user of {raw:?}");
        assert_eq!(got.host.unwrap_or(""), field("host"), "host of {raw:?}");
    }
}

#[test]
fn round_trips_a_tagged_privmsg() {
    let line = "@time=2026-11-05T12:00:00.000Z;msgid=a\\sb :nick!u@h PRIVMSG #chan :hello there";
    let message = Message::parse(line).unwrap();
    assert_eq!(message.tag("msgid"), Some("a b"));
    assert_eq!(message.source().unwrap().nick, "nick");
    assert_eq!(message.to_wire().unwrap(), format!("{line}\r\n"));
}

#[test]
fn uppercases_commands_on_parse() {
    assert_eq!(Message::parse("privmsg #a :b").unwrap().command, "PRIVMSG");
}

#[test]
fn refuses_to_serialize_anything_that_would_not_round_trip() {
    let cases: &[(&str, &[&str], WireError)] = &[
        ("PRIVMSG", &["#c", "hi\r\nQUIT :pwn"], WireError::LineBreak),
        ("PRIVMSG", &["#c\r\nQUIT", "x"], WireError::LineBreak),
        ("PRIVMSG", &["#c", "nul\0"], WireError::LineBreak),
        (
            "PRIVMSG",
            &["a b", "x"],
            WireError::Malformed("middle parameter"),
        ),
        (
            "PRIVMSG",
            &["", "x"],
            WireError::Malformed("middle parameter"),
        ),
        (
            "PRIVMSG",
            &[":a", "x"],
            WireError::Malformed("middle parameter"),
        ),
        ("", &[], WireError::Malformed("command")),
        ("PRIV MSG", &[], WireError::Malformed("command")),
    ];
    for (command, params, expected) in cases {
        let message = Message::new(command, params);
        assert_eq!(message.to_wire(), Err(expected.clone()), "{message:?}");
    }

    let mut message = Message::new("PING", &["x"]);
    message.source = Some("a b".into());
    assert_eq!(message.to_wire(), Err(WireError::Malformed("source")));
    message.source = None;
    message.tags = vec![("k=v".into(), String::new())];
    assert_eq!(message.to_wire(), Err(WireError::Malformed("tag key")));
}

#[test]
fn every_serialized_line_parses_back_to_itself() {
    let message = Message {
        tags: vec![("+draft/x".into(), "a;b c\\d\re\nf".into())],
        source: Some("n!u@h".into()),
        command: "PRIVMSG".into(),
        params: vec!["#c".into(), ":leading colon".into()],
    };
    let wire = message.to_wire().unwrap();
    assert_eq!(Message::parse(&wire).unwrap(), message);
}

#[test]
fn rejects_lines_without_a_command() {
    assert!(Message::parse("").is_err());
    assert!(Message::parse("\r\n").is_err());
    assert!(Message::parse(":source.only").is_err());
    assert!(Message::parse("@tag=1").is_err());
}
