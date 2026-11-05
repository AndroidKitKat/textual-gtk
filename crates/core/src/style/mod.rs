//! A Textual style on disk: its CSS and JS files, its settings, and the
//! template folders searched in Textual's order.

pub mod body;
pub mod template;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use template::{Template, TemplateError, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

/// The style's `Nickname Color Style` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NicknameColors {
    HslLight,
    HslDark,
}

#[derive(Debug)]
pub enum StyleError {
    Io(PathBuf, std::io::Error),
    Plist(PathBuf, plist::Error),
    Template(PathBuf, TemplateError),
    UnsupportedEngine(PathBuf),
}

impl fmt::Display for StyleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StyleError::Io(p, e) => write!(f, "{}: {e}", p.display()),
            StyleError::Plist(p, e) => write!(f, "{}: {e}", p.display()),
            StyleError::Template(p, e) => write!(f, "{}: {e}", p.display()),
            StyleError::UnsupportedEngine(p) => {
                write!(
                    f,
                    "{}: only template engine version 4 is supported",
                    p.display()
                )
            }
        }
    }
}

impl std::error::Error for StyleError {}

/// One folder of `.mustache` files. A partial resolves only inside the folder
/// of the template that includes it, as in GRMustache.
struct Repository {
    templates: HashMap<String, Template>,
}

impl Repository {
    fn load(dir: &Path) -> Result<Self, StyleError> {
        let mut templates = HashMap::new();
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Repository { templates });
            }
            Err(e) => return Err(StyleError::Io(dir.to_owned(), e)),
        };
        for entry in entries {
            let path = entry.map_err(|e| StyleError::Io(dir.to_owned(), e))?.path();
            let Some(name) = path
                .extension()
                .filter(|e| *e == "mustache")
                .and_then(|_| path.file_stem())
                .and_then(|s| s.to_str())
            else {
                continue;
            };
            let source =
                std::fs::read_to_string(&path).map_err(|e| StyleError::Io(path.clone(), e))?;
            let template =
                Template::parse(&source).map_err(|e| StyleError::Template(path.clone(), e))?;
            templates.insert(name.to_owned(), template);
        }
        Ok(Repository { templates })
    }
}

pub struct Style {
    /// `resources/styling`: holds `JavaScript/` and `Style Default Templates/`.
    pub resources: PathBuf,
    pub css_files: Vec<PathBuf>,
    pub js_files: Vec<PathBuf>,
    pub appearance: Appearance,
    pub nickname_colors: NicknameColors,
    /// The style's `Nickname Format`, if it sets one (`%n` nick, `%@` mode).
    pub nickname_format: Option<String>,
    /// The style's `Timestamp Format` (strftime), if it sets one.
    pub timestamp_format: Option<String>,
    /// Searched in order: the variety, the style root, the app defaults.
    repositories: Vec<Repository>,
}

impl Style {
    /// Loads a bundled style by folder name, picking the variety whose
    /// `Appearance` matches `want` when the style has varieties.
    pub fn load(resources: &Path, name: &str, want: Appearance) -> Result<Self, StyleError> {
        let root = resources.join("Bundled Styles").join(name);
        let root_settings = read_settings(&root.join("settings.plist"))?;
        let variety = pick_variety(&root, want)?;
        let variety_settings = match &variety {
            Some(dir) => read_settings(&dir.join("settings.plist"))?,
            None => plist::Dictionary::new(),
        };
        let setting = |key: &str| {
            variety_settings
                .get(key)
                .or_else(|| root_settings.get(key))
                .and_then(plist::Value::as_string)
                .map(str::to_owned)
        };

        let engine = root_settings
            .get("Template Engine Versions")
            .and_then(plist::Value::as_dictionary)
            .and_then(|d| d.get("default"))
            .and_then(plist::Value::as_unsigned_integer);
        if engine != Some(4) {
            return Err(StyleError::UnsupportedEngine(root));
        }

        let appearance = match setting("Appearance").as_deref() {
            Some("dark") => Appearance::Dark,
            Some("light") => Appearance::Light,
            _ => want,
        };
        let nickname_colors = match setting("Nickname Color Style").as_deref() {
            Some("HSL-dark") => NicknameColors::HslDark,
            Some("HSL-light") => NicknameColors::HslLight,
            _ if appearance == Appearance::Dark => NicknameColors::HslDark,
            _ => NicknameColors::HslLight,
        };

        let layers: Vec<&Path> = [Some(root.as_path()), variety.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        let existing = |file: &str| -> Vec<PathBuf> {
            layers
                .iter()
                .map(|dir| dir.join(file))
                .filter(|p| p.is_file())
                .collect()
        };

        let mut repositories = Vec::new();
        if let Some(dir) = &variety {
            repositories.push(Repository::load(&dir.join("Templates"))?);
        }
        repositories.push(Repository::load(&root.join("Templates"))?);
        repositories.push(Repository::load(
            &resources.join("Style Default Templates/Version 4"),
        )?);

        Ok(Style {
            resources: resources.to_owned(),
            css_files: existing("design.css"),
            js_files: existing("scripts.js"),
            appearance,
            nickname_colors,
            nickname_format: setting("Nickname Format"),
            timestamp_format: setting("Timestamp Format"),
            repositories,
        })
    }

    /// Renders a top-level template, with all newlines removed as Textual
    /// does. `None` if no folder has a template by that name.
    pub fn render(&self, name: &str, context: &Value) -> Option<String> {
        let repository = self
            .repositories
            .iter()
            .find(|r| r.templates.contains_key(name))?;
        let partial = |partial: &str| repository.templates.get(partial);
        let html = repository.templates[name].render(context, &partial);
        Some(html.replace(['\r', '\n'], ""))
    }

    /// The page a channel view loads: `baseLayout` with the keys Textual's
    /// `TVCLogController initialDocument` sets. `cache_token` busts WebKit's
    /// cache when the style is reloaded.
    pub fn initial_document(&self, channel: &str, font: &Font, cache_token: &str) -> String {
        let paths = |files: &[PathBuf]| {
            Value::List(files.iter().map(|p| Value::Str(file_url(p))).collect())
        };
        let appearance = match self.appearance {
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        };
        let context: Vec<(&str, Value)> = vec![
            ("userConfiguredTextEncoding", "utf-8".into()),
            ("activeStyleCSSFiles", paths(&self.css_files)),
            ("activeStyleJSFiles", paths(&self.js_files)),
            ("cacheToken", cache_token.into()),
            (
                "applicationTemplatesPath",
                file_url(&self.resources.join("Style Default Templates/Version 4")).into(),
            ),
            ("applicationResourcePath", file_url(&self.resources).into()),
            ("userConfiguredFontName", font.family.clone().into()),
            (
                "userConfiguredFontSize",
                format!("{}", font.points * 72.0 / 96.0).into(),
            ),
            ("nicknameIndentationAvailable", false.into()),
            ("userStyleSheetRules", "".into()),
            ("textDirectionToken", "ltr".into()),
            ("appearanceToken", appearance.into()),
            ("usesCustomScrollers", false.into()),
            ("operatingSystemVersion", "linux".into()),
            ("isChannelView", true.into()),
            ("channelName", channel.into()),
            ("viewTypeToken", "channel".into()),
        ];
        let context = Value::Map(
            context
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        );
        self.render("baseLayout", &context).unwrap_or_default()
    }
}

/// The chat font. Textual's default is 12 pt.
#[derive(Debug, Clone)]
pub struct Font {
    pub family: String,
    pub points: f64,
}

/// A `file://` URL with every byte outside the unreserved set and `/`
/// percent-encoded, so it is safe in attributes and JS string literals.
pub fn file_url(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut url = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            url.push(byte as char);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

fn read_settings(path: &Path) -> Result<plist::Dictionary, StyleError> {
    if !path.is_file() {
        return Ok(plist::Dictionary::new());
    }
    let value = plist::Value::from_file(path).map_err(|e| StyleError::Plist(path.to_owned(), e))?;
    Ok(value.into_dictionary().unwrap_or_default())
}

fn pick_variety(root: &Path, want: Appearance) -> Result<Option<PathBuf>, StyleError> {
    let dir = root.join("Varieties");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(StyleError::Io(dir, e)),
    };
    let mut varieties: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    varieties.sort();
    let wanted = match want {
        Appearance::Light => "light",
        Appearance::Dark => "dark",
    };
    for variety in &varieties {
        let settings = read_settings(&variety.join("settings.plist"))?;
        if settings.get("Appearance").and_then(plist::Value::as_string) == Some(wanted) {
            return Ok(Some(variety.clone()));
        }
    }
    Ok(varieties.into_iter().next())
}
