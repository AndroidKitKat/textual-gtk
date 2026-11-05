use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use textual_gtk::irc::Message;
use textual_gtk::irc::connection::{self, Command, Config, Disconnect, Event};
use textual_gtk::line::{Destination, Line, LineType};
use textual_gtk::style::{Appearance, Style};
use textual_gtk::ui::chat_view::{self, ChatView};

const APP_ID: &str = "io.github.androidkitkat.TextualGtk";
const QUIT_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Phase-1 settings, from the command line:
/// `textual-gtk [--server host:port] [--nick nick] [--channel #chan] [--style name]`.
#[derive(Debug, Clone)]
struct Options {
    host: String,
    port: u16,
    nick: String,
    channel: String,
    style: String,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        host: "127.0.0.1".into(),
        port: 6667,
        nick: "textual".into(),
        channel: "#textual".into(),
        style: "Simplified".into(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--server" => {
                let server = value()?;
                let (host, port) = server.rsplit_once(':').unwrap_or((&server, "6667"));
                options.host = host.to_owned();
                options.port = port.parse().map_err(|_| format!("bad port in {server}"))?;
            }
            "--nick" => options.nick = value()?,
            "--channel" => options.channel = value()?,
            "--style" => options.style = value()?,
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(options)
}

fn resources_dir() -> PathBuf {
    std::env::var_os("TEXTUAL_GTK_RESOURCES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/styling")))
}

fn main() -> glib::ExitCode {
    let options = match parse_options() {
        Ok(options) => options,
        Err(e) => {
            eprintln!("textual-gtk: {e}");
            return glib::ExitCode::FAILURE;
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
    app.connect_activate(move |app| {
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
        build_window(app, &options, style, &pages_for_app, &tokio_handle);
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

fn build_window(
    app: &adw::Application,
    options: &Options,
    style: Rc<Style>,
    pages: &std::path::Path,
    runtime: &tokio::runtime::Handle,
) {
    let view = ChatView::new(&style, &options.channel, pages);

    let entry = gtk::Entry::builder()
        .placeholder_text(format!("Message {}", options.channel))
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .sensitive(false)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&view.widget());
    content.append(&entry);

    let title = adw::WindowTitle::new(
        &options.channel,
        &format!("{}:{}", options.host, options.port),
    );
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Textual")
        .default_width(900)
        .default_height(600)
        .content(&toolbar)
        .build();
    if let Some(prefix) = std::env::var_os("TEXTUAL_GTK_PROBE") {
        install_probe(&view, &window, PathBuf::from(prefix));
    }

    let connection = connection::spawn(
        runtime,
        Config {
            host: options.host.clone(),
            port: options.port,
            nick: options.nick.clone(),
            username: options.nick.clone(),
            realname: "Textual for Linux".into(),
            channels: vec![options.channel.clone()],
        },
    );
    let commands = connection.commands.clone();
    let own_nick = Rc::new(RefCell::new(options.nick.clone()));

    let print = {
        let view = view.clone();
        let style = style.clone();
        move |line: Line| view.append(line.render(&style), &line.id)
    };

    {
        let channel = options.channel.clone();
        let own_nick = own_nick.clone();
        let print = print.clone();
        entry.connect_activate(move |entry| {
            let text = entry.text().to_string();
            if text.is_empty() {
                return;
            }
            entry.set_text("");
            for part in text.lines().filter(|l| !l.is_empty()) {
                let message = Message::new("PRIVMSG", &[&channel, part]);
                if commands.send(Command::Send(message)).is_err() {
                    return;
                }
                let mut line = Line::new(
                    LineType::Privmsg,
                    "PRIVMSG",
                    Some(&own_nick.borrow()),
                    part.to_owned(),
                );
                line.from_self = true;
                print(line);
            }
        });
    }

    let quitting = Rc::new(std::cell::Cell::new(false));
    let channel = options.channel.clone();
    let view = view.clone();
    let mut events = connection.events;
    let entry = entry.clone();
    let window_for_events = window.clone();
    let quitting_for_events = quitting.clone();
    glib::spawn_future_local(async move {
        while let Some(event) = events.recv().await {
            match event {
                Event::Connected => {}
                Event::Refused { message, reason } => print(Line::new(
                    LineType::Debug,
                    "-100",
                    None,
                    format!("Not sent ({reason}): {}", message.params.join(" ")),
                )),
                Event::Registered { nick } => {
                    *own_nick.borrow_mut() = nick;
                    entry.set_sensitive(true);
                    entry.grab_focus();
                }
                Event::Message(message) => {
                    let current = own_nick.borrow().clone();
                    let (topic_channel, topic) = match message.command.as_str() {
                        "331" => (1, Some("")),
                        "332" => (1, message.param(2)),
                        "TOPIC" => (0, message.param(1)),
                        _ => (0, None),
                    };
                    if let Some(topic) = topic
                        && message
                            .param(topic_channel)
                            .is_some_and(|c| c.eq_ignore_ascii_case(&channel))
                    {
                        view.set_topic(topic, &style);
                    }
                    if message.command == "NICK"
                        && message
                            .source()
                            .is_some_and(|s| s.nick.eq_ignore_ascii_case(&current))
                        && let Some(new) = message.param(0)
                    {
                        *own_nick.borrow_mut() = new.to_owned();
                    }
                    let Some((destination, line)) = Line::from_message(&message, &current) else {
                        continue;
                    };
                    let shown = match &destination {
                        Destination::Channel(name) => name.eq_ignore_ascii_case(&channel),
                        Destination::ChannelsOf(_) => true,
                        Destination::Server => false,
                    };
                    if shown {
                        print(line);
                    }
                }
                Event::Disconnected(reason) => {
                    entry.set_sensitive(false);
                    if quitting_for_events.get() {
                        window_for_events.destroy();
                        return;
                    }
                    let text = match reason {
                        Disconnect::UserQuit => "Disconnected from IRC".to_owned(),
                        Disconnect::ServerClosed => {
                            "Disconnected from IRC: the server closed the connection".to_owned()
                        }
                        Disconnect::Error(e) => format!("Disconnected from IRC: {e}"),
                    };
                    print(Line::new(LineType::Debug, "-100", None, text));
                }
            }
        }
    });

    // Closing sends QUIT and waits (up to QUIT_WAIT) for the connection to
    // end, so the server sees a clean quit before the process exits.
    let quit_commands = connection.commands;
    window.connect_close_request(move |window| {
        if quitting.replace(true) || quit_commands.send(Command::Quit(None)).is_err() {
            return glib::Propagation::Proceed;
        }
        window.set_visible(false);
        let window = window.clone();
        glib::timeout_add_local_once(QUIT_WAIT, move || window.destroy());
        glib::Propagation::Stop
    });
    window.present();
}

/// For the end-to-end check: once a second, writes what the page shows to
/// `<prefix>.json` and a snapshot to `<prefix>.png`. Creating
/// `<prefix>.reload` reloads the page; `<prefix>.close` closes the window.
fn install_probe(view: &ChatView, window: &adw::ApplicationWindow, prefix: PathBuf) {
    const STATE: &str = "({\
        lines: document.querySelectorAll('#messageBuffer > .line').length,\
        last: (document.querySelector('#messageBuffer > .line:last-child') || {}).innerText || '',\
        lastHtml: (document.querySelector('#messageBuffer > .line:last-child') || {}).innerHTML || '',\
        bodyOpacity: getComputedStyle(document.getElementById('body')).opacity,\
        topic: (document.getElementById('topicBar') || {}).innerText || '',\
        loadedAt: performance.timeOrigin\
    })";
    let view = view.clone();
    let window = window.clone();
    glib::timeout_add_seconds_local(1, move || {
        let trigger = |suffix: &str| std::fs::remove_file(prefix.with_extension(suffix)).is_ok();
        if trigger("reload") {
            webkit6::prelude::WebViewExt::reload(&view.widget());
        }
        if trigger("close") {
            window.close();
        }
        let json_path = prefix.with_extension("json");
        let png_path = prefix.with_extension("png");
        view.query(STATE, move |state| {
            if let Some(state) = state {
                let _ = std::fs::write(&json_path, state);
            }
        });
        webkit6::prelude::WebViewExt::snapshot(
            &view.widget(),
            webkit6::SnapshotRegion::Visible,
            webkit6::SnapshotOptions::NONE,
            None::<&gtk::gio::Cancellable>,
            move |texture| {
                if let Ok(texture) = texture {
                    let _ = texture.save_to_png(&png_path);
                }
            },
        );
        glib::ControlFlow::Continue
    });
}
