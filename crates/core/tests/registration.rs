mod support;

use support::{Script, setup};
use textual_core::irc::caps::WANTED;
use textual_core::model::registration::{Output, Progress, Registration, SetupError};
use textual_core::secret::Secret;

#[test]
fn a_nick_with_a_space_is_refused_before_anything_is_sent() {
    let mut out = Output::default();
    let err = Registration::start(setup("two words"), WANTED, &mut out).unwrap_err();
    assert_eq!(err, SetupError::BadNick("two words".into()));
    assert!(out.lines.is_empty());

    let mut bad_alt = setup("textual");
    bad_alt.alternates = vec![":colon".into()];
    let err = Registration::start(bad_alt, WANTED, &mut out).unwrap_err();
    assert_eq!(err, SetupError::BadNick(":colon".into()));
}

#[test]
fn a_bad_username_is_refused() {
    let mut s = setup("textual");
    s.username = "a b".into();
    let mut out = Output::default();
    assert_eq!(
        Registration::start(s, WANTED, &mut out).unwrap_err(),
        SetupError::BadUsername("a b".into())
    );
}

#[test]
fn registration_debug_does_not_print_secrets() {
    let mut s = setup("textual");
    s.server_password = Some(Secret::new("serverpw"));
    s.account_password = Some(Secret::new("accountpw"));
    let t = Script::new(s);
    let shown = format!("{:?}", t.state());
    assert!(
        !shown.contains("serverpw") && !shown.contains("accountpw"),
        "{shown}"
    );
}

#[test]
fn caps_stay_live_after_001() {
    let mut t = Script::with_caps(setup("textual"), &["away-notify", "multi-prefix"]);
    t.sent(["CAP LS 302", "NICK textual", "USER textual 0 * Textual"]);
    t.recv(":s CAP * LS :multi-prefix");
    t.sent(["CAP REQ multi-prefix"]);
    t.recv(":s CAP * ACK multi-prefix");
    t.sent(["CAP END"]);
    t.recv(":s 001 textual :Welcome");
    let Progress::Registered(reg) = t.state() else {
        panic!("not registered: {:?}", t.state());
    };
    let mut caps = reg.caps.clone();
    let step =
        caps.on_cap(&textual_core::irc::Message::parse(":s CAP textual NEW :away-notify").unwrap());
    let sent: Vec<&str> = step.send.iter().map(|l| l.as_str()).collect();
    assert_eq!(sent, ["CAP REQ away-notify\r\n"]);
}

#[test]
fn output_debug_does_not_print_secrets() {
    let mut s = setup("textual");
    s.server_password = Some(Secret::new("serverpw"));
    let mut out = Output::default();
    Registration::start(s, WANTED, &mut out).unwrap();
    let shown = format!("{out:?}");
    assert!(!shown.contains("serverpw"), "{shown}");
    assert!(shown.contains("NICK textual"), "{shown}");
}
