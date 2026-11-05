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
    let pages = glib::user_cache_dir().join("textual-gtk/pages");
    let _ = std::fs::remove_dir_all(&pages);
    if let Err(e) = std::fs::create_dir_all(&pages) {
        eprintln!("textual-gtk: cannot create {}: {e}", pages.display());
        return glib::ExitCode::FAILURE;
    }
    chat_view::prepare_web_context(&resources, &pages);

    let app = adw::Application::builder().application_id(APP_ID).build();
    let tokio_handle = runtime.handle().clone();
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
        build_window(app, &options, style, &pages, &tokio_handle);
    });
    app.run_with_args::<&str>(&[])
}

fn build_window(
    app: &adw::Application,
    options: &Options,
    style: Rc<Style>,
    pages: &std::path::Path,
    runtime: &tokio::runtime::Handle,
) {
    let view = ChatView::new(&style, &options.channel, "", pages);
    if let Some(prefix) = std::env::var_os("TEXTUAL_GTK_PROBE") {
        install_probe(&view, PathBuf::from(prefix));
    }

    let entry = gtk::Entry::builder()
        .placeholder_text(format!("Message {}", options.channel))
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
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

    let channel = options.channel.clone();
    let view = view.clone();
    let mut events = connection.events;
    glib::spawn_future_local(async move {
        while let Some(event) = events.recv().await {
            match event {
                Event::Connected | Event::Refused { .. } => {}
                Event::Registered { nick } => *own_nick.borrow_mut() = nick,
                Event::Message(message) => {
                    let current = own_nick.borrow().clone();
                    let topic = match message.command.as_str() {
                        "332" => message.param(2),
                        "TOPIC" => message.param(1),
                        _ => None,
                    };
                    let topic_channel = if message.command == "332" { 1 } else { 0 };
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

    let quit_commands = connection.commands;
    window.connect_close_request(move |_| {
        let _ = quit_commands.send(Command::Quit(None));
        glib::Propagation::Proceed
    });
    window.present();
}

/// For the end-to-end check: once a second, writes what the page shows to
/// `<prefix>.json` and a snapshot to `<prefix>.png`.
fn install_probe(view: &ChatView, prefix: PathBuf) {
    const STATE: &str = "({\
        lines: document.querySelectorAll('#messageBuffer > .line').length,\
        last: (document.querySelector('#messageBuffer > .line:last-child') || {}).innerText || '',\
        bodyOpacity: getComputedStyle(document.getElementById('body')).opacity,\
        topic: (document.getElementById('topicBar') || {}).innerText || ''\
    })";
    let view = view.clone();
    glib::timeout_add_seconds_local(1, move || {
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
