use std::path::{Path, PathBuf};

use textual_core::style::template::{Template, Value};

fn templates_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            templates_under(&path, out);
        } else if path.extension().is_some_and(|e| e == "mustache") {
            out.push(path);
        }
    }
}

fn styling() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/styling")
}

#[test]
fn parses_every_bundled_template() {
    let mut paths = Vec::new();
    templates_under(&styling(), &mut paths);
    assert_eq!(paths.len(), 22, "{paths:#?}");
    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap();
        if let Err(e) = Template::parse(&source) {
            panic!("{}: {e}", path.display());
        }
    }
}

#[test]
fn fragment_template_uses_style_attribute_only_for_hex_colors() {
    let path =
        styling().join("Style Default Templates/Version 4/formattedMessageFragment.mustache");
    let template = Template::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
    let ctx = |uses_style: bool| {
        Value::Map(
            [
                ("fragmentIsBoldOpened", Value::from(true)),
                ("fragmentTextColorOpened", true.into()),
                ("fragmentTextColorUsesStyleTag", uses_style.into()),
                ("fragmentForegroundColorIsSet", true.into()),
                ("fragmentForegroundColor", "#ff0000".into()),
                ("messageFragment", "hi &amp; bye".into()),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
        )
    };
    let hex = template
        .render(&ctx(true), &|_| None)
        .replace(['\n', '\r'], "");
    let mirc = template
        .render(&ctx(false), &|_| None)
        .replace(['\n', '\r'], "");
    assert!(hex.contains("color: #ff0000"), "{hex}");
    assert!(!hex.contains("data-foreground-color"), "{hex}");
    assert!(mirc.contains("data-foreground-color=\"#ff0000\""), "{mirc}");
    assert!(!mirc.contains("style="), "{mirc}");
    assert!(hex.contains("<b>hi &amp; bye"), "{hex}");
}
