use std::path::{Path, PathBuf};

use chrono::{Local, TimeZone};
use textual_core::irc::Message;
use textual_core::irc::isupport::Isupport;
use textual_core::line::{Line, LineId, LineType, Stamp};
use textual_core::style::{Appearance, Font, Style};

fn resources() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/styling")
}

fn simplified() -> Style {
    Style::load(&resources(), "Simplified", Appearance::Light).unwrap()
}

fn stamp() -> Stamp {
    Stamp {
        id: LineId::new(7),
        at: Local.timestamp_opt(1_800_000_000, 0).unwrap(),
    }
}

fn render(raw: &str) -> String {
    let message = Message::parse(raw).unwrap();
    let (_, line) = Line::from_message(&message, "me", &Isupport::default(), stamp()).unwrap();
    line.render(&simplified())
}

#[test]
fn loads_every_bundled_style_in_both_appearances() {
    let styles = [
        "Astria",
        "Equinox",
        "Sapientia",
        "Simplified",
        "Sulaco",
        "Tomorrow",
        "Total Sublime",
    ];
    for name in styles {
        for want in [Appearance::Light, Appearance::Dark] {
            let style =
                Style::load(&resources(), name, want).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!style.css_files.is_empty(), "{name} has no CSS");
            let html = Line::new(stamp(), LineType::Privmsg, "PRIVMSG", Some("a"), "b".into())
                .render(&style);
            assert!(html.contains("class=\"line"), "{name}: {html}");
        }
    }
}

#[test]
fn simplified_light_picks_the_light_variety() {
    let style = simplified();
    assert_eq!(style.appearance, Appearance::Light);
    assert!(
        style.css_files[0].ends_with("Varieties/Light/design.css"),
        "{:?}",
        style.css_files
    );
    assert!(
        style.js_files[0].ends_with("Simplified/scripts.js"),
        "{:?}",
        style.js_files
    );
}

#[test]
fn a_privmsg_renders_with_sender_escaping_and_formatting() {
    let html = render(":alice!a@h PRIVMSG #c :<b>hi</b> \x02bold\x02 me");
    assert!(!html.contains('\n'), "newlines are stripped");
    assert!(html.contains("class=\"line text\""), "{html}");
    assert!(html.contains("data-nickname=\"alice\""), "{html}");
    assert!(
        html.contains("&lt;alice&gt;"),
        "default nickname format: {html}"
    );
    assert!(
        html.contains("&lt;b&gt;hi&lt;/b&gt;"),
        "body is escaped: {html}"
    );
    assert!(html.contains("<b>bold</b>"), "bold rendered: {html}");
    assert!(
        html.contains("highlight=\"true\""),
        "mention highlights: {html}"
    );
    assert!(html.contains("color: hsl("), "nickname colour: {html}");
}

#[test]
fn a_join_renders_as_an_event_line() {
    let html = render(":bob!b@host JOIN #c");
    assert!(html.contains("class=\"line event\""), "{html}");
    assert!(html.contains("data-line-type=\"join\""), "{html}");
    assert!(
        html.contains("<b>bob</b>&nbsp;(b@host) joined the channel"),
        "{html}"
    );
}

#[test]
fn the_initial_document_loads_the_style_and_textual_js() {
    let style = simplified();
    let font = Font {
        family: "Sans".into(),
        points: 12.0,
    };
    let html = style.initial_document("#textual", &font, "42");
    let css = textual_core::style::file_url(&style.css_files[0]);
    assert!(
        css.starts_with("file:///") && css.contains("Bundled%20Styles"),
        "{css}"
    );
    assert!(html.contains(&format!("href=\"{css}?42\"")), "{html}");
    assert!(html.contains("JavaScript/API/core.js"), "{html}");
    assert!(html.contains("Textual.initializeCore(\""), "{html}");
    assert!(html.contains("data-view-name=\"#textual\""), "{html}");
    assert!(html.contains("id=\"topicBar\""), "{html}");
    assert!(html.contains("font-size: 9pt"), "{html}");
}
