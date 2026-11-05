mod support;

use std::path::Path;

#[test]
fn every_scenario_transcript_passes() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scenarios");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/scenarios exists")
        .map(|e| e.expect("readable entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "irc"))
        .collect();
    paths.sort();
    assert!(paths.len() >= 29, "scenarios went missing: {paths:?}");

    let mut failures = String::new();
    for path in &paths {
        let source = std::fs::read_to_string(path).expect("readable scenario");
        if let Err(errors) = support::run_transcript(&source) {
            let name = path.file_name().unwrap().to_string_lossy();
            failures.push_str(&format!("{name}:\n{errors}\n"));
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
}

#[test]
fn the_runner_reports_a_wrong_sent_line() {
    let errors = support::run_transcript("> CAP LS 301\n> NICK textual\n").unwrap_err();
    assert!(
        errors.contains("1: expected sent \"CAP LS 301\", got \"CAP LS 302\""),
        "{errors}"
    );
    assert!(
        errors.contains("unexpected sent \"USER textual 0 * Textual\""),
        "{errors}"
    );
}

#[test]
fn the_runner_reports_an_unchecked_notice_and_bad_time() {
    let unchecked = "= auth sasl-plain\n> CAP LS 302\n> NICK textual\n> USER textual 0 * Textual\n< :s CAP * LS :x\n> CAP END\n? registering\n";
    let errors = support::run_transcript(unchecked).unwrap_err();
    assert!(
        errors.contains("unchecked notice \"SASL failed: the server does not offer SASL"),
        "{errors}"
    );
    let errors = support::run_transcript("! connect\n@ 5s\n").unwrap_err();
    assert!(errors.contains("2: bad time \"5s\""), "{errors}");
}

#[test]
fn the_runner_reports_a_wrong_wait_with_the_real_one() {
    let script = "! connect\n> dial #1 irc.test:6667 plain\n? notice Connecting to irc.test:6667\n! closed eof\n? notice Disconnected: the server closed the connection; reconnecting in 5 s\n? waiting 5s\n";
    let errors = support::run_transcript(script).unwrap_err();
    assert!(
        errors.contains("6: expected \"waiting 5s\", got waiting 4.965s"),
        "{errors}"
    );
}

#[test]
fn a_scenario_without_an_end_state_fails() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scenarios/register-plain.irc");
    let source = std::fs::read_to_string(path).expect("readable scenario");
    let truncated: Vec<&str> = source.lines().collect();
    let (last, rest) = truncated.split_last().unwrap();
    assert!(last.starts_with("? registered"), "{last}");
    let errors = support::run_transcript(&rest.join("\n")).unwrap_err();
    assert!(errors.contains("end: no end state"), "{errors}");
    let registering = "> CAP LS 302\n> NICK textual\n> USER textual 0 * Textual\n? registering\n";
    assert_eq!(support::run_transcript(registering), Ok(()));
}
