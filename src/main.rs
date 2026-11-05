use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use textual_core::irc::caps::WANTED;
use textual_core::model::network::{
    Address, ChannelConfig, Input as NetInput, ServerConfig, ServerId, Tls,
};
use textual_core::model::registration::{Auth, Setup};
use textual_core::model::workspace::{Input, Out, Workspace};
use textual_core::style::{Appearance, Style};
use textual_gtk::app;
use textual_gtk::io::Io;
use textual_gtk::ui::chat_view;
use textual_gtk::ui::probe;
use textual_gtk::ui::view::View;

const APP_ID: &str = "io.github.androidkitkat.TextualGtk";
const USAGE: &str = "usage: textual-gtk --network 'name=Local,host=127.0.0.1,port=6667,tls=off,nick=me,join=#a #b' [--network ...] [--style name]
network keys: name, host, nick (required); port (6667, or 6697 with tls=on); tls=off|on; verify=true|false (with tls=on); user (the nick); realname; join (space-separated channels, #chan:key for a key)";

#[derive(Debug)]
struct Options {
    networks: Vec<ServerConfig>,
    style: String,
}

fn parse_options(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut networks = Vec::new();
    let mut style = "Simplified".to_owned();
    let mut args = args;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--network" => networks.push(parse_network(&value, networks.len() as u128 + 1)?),
            "--style" => style = value,
            other => return Err(format!("unknown option {other}")),
        }
    }
    if networks.is_empty() {
        return Err(USAGE.to_owned());
    }
    for (i, net) in networks.iter().enumerate() {
        if networks[..i].iter().any(|n| n.name == net.name) {
            return Err(format!("two networks are named {}", net.name));
        }
    }
    Ok(Options { networks, style })
}

/// `key=value,key=value`; `join` takes a space-separated list.
fn parse_network(spec: &str, index: u128) -> Result<ServerConfig, String> {
    let mut fields = std::collections::HashMap::new();
    for pair in spec.split(',').filter(|p| !p.trim().is_empty()) {
        let (key, value) = pair
            .split_once('=')
            .ok_or(format!("{pair:?} is not key=value"))?;
        fields.insert(key.trim(), value.trim());
    }
    let required = |key: &str| {
        fields
            .get(key)
            .filter(|v| !v.is_empty())
            .map(|v| (*v).to_owned())
            .ok_or(format!("--network needs {key}="))
    };
    let name = required("name")?;
    let host = required("host")?;
    let nick = required("nick")?;
    let tls = match fields.get("tls").copied().unwrap_or("off") {
        "off" => Tls::Off,
        "on" => Tls::On {
            verify: match fields.get("verify").copied().unwrap_or("true") {
                "true" => true,
                "false" => false,
                other => return Err(format!("verify={other}: expected true or false")),
            },
        },
        other => return Err(format!("tls={other}: expected on or off")),
    };
    let port = match fields.get("port") {
        Some(p) => p.parse().map_err(|_| format!("bad port {p}"))?,
        None if matches!(tls, Tls::On { .. }) => 6697,
        None => 6667,
    };
    let autojoin = fields
        .get("join")
        .map(|list| {
            list.split_whitespace()
                .map(|entry| {
                    let (name, key) = entry
                        .split_once(':')
                        .map_or((entry, None), |(n, k)| (n, Some(k.to_owned())));
                    ChannelConfig {
                        name: name.to_owned(),
                        key,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ServerConfig {
        id: ServerId(index),
        name,
        addresses: vec![Address { host, port }],
        tls,
        auto_reconnect: true,
        setup: Setup {
            username: fields
                .get("user")
                .map_or_else(|| nick.clone(), |u| (*u).to_owned()),
            nick,
            alternates: Vec::new(),
            realname: fields
                .get("realname")
                .map_or("Textual GTK", |r| r)
                .to_owned(),
            auth: Auth::None,
            server_password: None,
            account_password: None,
        },
        autojoin,
        wanted_caps: WANTED.iter().map(|c| (*c).to_owned()).collect(),
    })
}

fn resources_dir() -> PathBuf {
    std::env::var_os("TEXTUAL_GTK_RESOURCES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/styling")))
}

fn main() -> glib::ExitCode {
    let options = match parse_options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(e) => {
            eprintln!("textual-gtk: {e}");
            return glib::ExitCode::from(2);
        }
    };
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let resources = resources_dir();
    let pages = match pages_dir() {
        Ok(pages) => pages,
        Err(e) => {
            eprintln!("textual-gtk: cannot create the page folder: {e}");
            return glib::ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&pages) {
        eprintln!("textual-gtk: cannot create {}: {e}", pages.display());
        return glib::ExitCode::FAILURE;
    }
    chat_view::prepare_web_context(&resources, &pages);

    let app = adw::Application::builder().application_id(APP_ID).build();
    let tokio_handle = runtime.handle().clone();
    let pages_for_app = pages.clone();
    let networks = std::cell::RefCell::new(Some(options.networks));
    app.connect_activate(move |app| {
        let Some(networks) = networks.take() else {
            return;
        };
        let appearance = if adw::StyleManager::default().is_dark() {
            Appearance::Dark
        } else {
            Appearance::Light
        };
        let style = match Style::load(&resources, &options.style, appearance) {
            Ok(style) => Rc::new(style),
            Err(e) => {
                eprintln!("textual-gtk: cannot load style {}: {e}", options.style);
                app.quit();
                return;
            }
        };
        start(app, networks, style, &pages_for_app, &tokio_handle);
    });
    let status = app.run_with_args::<&str>(&[]);
    let _ = std::fs::remove_dir_all(&pages);
    status
}

/// A page folder per process, so a second instance or a test run never
/// deletes the page a running instance shows. Folders left by processes
/// that no longer exist are removed.
fn pages_dir() -> std::io::Result<PathBuf> {
    let root = glib::user_cache_dir().join("textual-gtk/pages");
    std::fs::create_dir_all(&root)?;
    for entry in std::fs::read_dir(&root)?.flatten() {
        let stale = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .is_some_and(|pid| !std::path::Path::new(&format!("/proc/{pid}")).exists());
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    Ok(root.join(std::process::id().to_string()))
}

/// The design's "Usage" block: build the model, the view and the I/O, then
/// hand them to the one loop.
fn start(
    app: &adw::Application,
    networks: Vec<ServerConfig>,
    style: Rc<Style>,
    pages: &std::path::Path,
    runtime: &tokio::runtime::Handle,
) {
    // The loop sleeps with tokio::time::sleep_until on the GTK thread,
    // which needs the runtime entered there for as long as the app runs.
    let handle: &'static tokio::runtime::Handle = Box::leak(Box::new(runtime.clone()));
    std::mem::forget(handle.enter());

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut workspace = Workspace::new();
    let mut out = Out::default();
    let servers: Vec<ServerId> = networks.iter().map(|n| n.id).collect();
    for config in networks {
        workspace.add_network(config, &mut out);
    }
    let mut view = View::build(app, style, tx.clone(), pages);
    view.apply(&workspace, &out.changes);
    let io = Io::new(runtime.clone(), tx.clone());
    for server in servers {
        let _ = tx.send(
            Input::Network {
                server,
                input: NetInput::Connect,
            }
            .into(),
        );
    }
    let probe_prefix = std::env::var_os("TEXTUAL_GTK_PROBE").map(PathBuf::from);
    if let Some(prefix) = &probe_prefix {
        probe::install(prefix.clone(), tx.clone());
    }
    glib::spawn_future_local(app::run(workspace, view, io, rx, probe_prefix));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        parse_options(args.iter().map(|a| (*a).to_owned()))
    }

    #[test]
    fn a_network_spec_fills_the_config() {
        let options = parse(&[
            "--network",
            "name=Local,host=127.0.0.1,nick=me,join=#a #b:key",
            "--network",
            "name=Secure,host=irc.example,tls=on,verify=false,nick=mes,user=u,realname=R",
            "--style",
            "Other",
        ])
        .unwrap();
        assert_eq!(options.style, "Other");
        let [local, secure] = options.networks.as_slice() else {
            panic!("two networks");
        };
        assert_eq!(local.id, ServerId(1));
        assert_eq!(local.name, "Local");
        assert_eq!(local.addresses[0].port, 6667);
        assert_eq!(local.tls, Tls::Off);
        assert_eq!(local.setup.nick, "me");
        assert_eq!(local.setup.username, "me");
        assert_eq!(local.setup.realname, "Textual GTK");
        assert_eq!(
            local.autojoin,
            [
                ChannelConfig {
                    name: "#a".into(),
                    key: None
                },
                ChannelConfig {
                    name: "#b".into(),
                    key: Some("key".into())
                }
            ]
        );
        assert_eq!(secure.id, ServerId(2));
        assert_eq!(secure.addresses[0].port, 6697);
        assert_eq!(secure.tls, Tls::On { verify: false });
        assert_eq!(secure.setup.username, "u");
        assert_eq!(secure.setup.realname, "R");
        assert!(secure.autojoin.is_empty());
    }

    #[test]
    fn bad_specs_are_refused() {
        assert!(parse(&[]).unwrap_err().starts_with("usage:"));
        assert!(parse(&["--network", "host=h,nick=n"]).is_err(), "no name");
        assert!(parse(&["--network", "name=a,host=h"]).is_err(), "no nick");
        assert!(
            parse(&["--network", "name=a,host=h,nick=n,tls=maybe"]).is_err(),
            "bad tls"
        );
        assert!(
            parse(&["--network", "name=a,host=h,nick=n,port=x"]).is_err(),
            "bad port"
        );
        let twice = parse(&[
            "--network",
            "name=a,host=h,nick=n",
            "--network",
            "name=a,host=h2,nick=n",
        ]);
        assert_eq!(twice.unwrap_err(), "two networks are named a");
    }
}
