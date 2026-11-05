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
    assert!(paths.len() >= 15, "scenarios went missing: {paths:?}");

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
fn the_runner_reports_an_unchecked_notice_and_rejects_time() {
    let unchecked = "= auth sasl-plain\n< :s CAP * LS :x\n";
    let errors = support::run_transcript(unchecked).unwrap_err();
    assert!(
        errors.contains("unchecked notice \"SASL failed: the server does not offer SASL"),
        "{errors}"
    );
    assert!(support::run_transcript("> CAP LS 302\n@ +5s\n").is_err());
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
