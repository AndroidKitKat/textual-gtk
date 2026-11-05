//! A channel's message view: a WebKitGTK page built from the style's
//! `baseLayout`, driven through Textual's own JavaScript API.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::glib;
use webkit6::prelude::*;
use webkit6::{LoadEvent, Settings, UserContentManager, WebContext, WebView};

use crate::style::{Font, Style};

/// The 40 handlers Textual registers (`TVCLogViewInternalWK2.m:95-134`).
/// Simplified needs only `finishedLayingOutView` and the two line
/// notifications; registering the rest keeps `app.*` calls from throwing,
/// and any call that waits for an answer gets `null`.
const HANDLERS: &[&str] = &[
    "appearance",
    "channelIsActive",
    "channelMemberCount",
    "channelName",
    "channelNameDoubleClicked",
    "displayContextMenu",
    "copySelectionWhenPermitted",
    "encryptionAuthenticateUser",
    "inlineMediaEnabledForView",
    "loadInlineMedia",
    "localUserHostmask",
    "localUserNickname",
    "logToConsole",
    "networkName",
    "nicknameColorStyleHash",
    "nicknameDoubleClicked",
    "notifyLinesAddedToView",
    "notifyLinesRemovedFromView",
    "notifyJumpToLineCallback",
    "printDebugInformation",
    "printDebugInformationToConsole",
    "renderMessagesBefore",
    "renderMessagesAfter",
    "renderMessagesInRange",
    "renderMessageWithSiblings",
    "renderTemplate",
    "retrievePreferencesWithMethodName",
    "sendPluginPayload",
    "serverAddress",
    "serverChannelCount",
    "serverIsConnected",
    "setChannelName",
    "setNickname",
    "setSelection",
    "setURLAddress",
    "sidebarInversionIsEnabled",
    "styleSettingsRetrieveValue",
    "styleSettingsSetValue",
    "topicBarDoubleClicked",
    "finishedLayingOutView",
];

enum Readiness {
    /// Lines that arrived before Textual's JS finished loading; Textual
    /// drops calls made then, so they wait here.
    Loading(Vec<String>),
    Ready,
}

struct Inner {
    view: WebView,
    readiness: Readiness,
}

#[derive(Clone)]
pub struct ChatView {
    inner: Rc<RefCell<Inner>>,
}

/// Lets the sandboxed web process read the style resources and the page
/// files. Must run before the first view is created.
pub fn prepare_web_context(resources: &Path, pages: &Path) {
    let context = WebContext::default().expect("WebKit has a default context");
    context.add_path_to_sandbox(resources, true);
    context.add_path_to_sandbox(pages, true);
}

impl ChatView {
    pub fn new(style: &Style, channel: &str, topic: &str, pages: &Path) -> Self {
        let settings = Settings::new();
        settings.set_allow_file_access_from_file_urls(true);
        settings.set_allow_universal_access_from_file_urls(true);
        settings.set_enable_developer_extras(true);
        settings.set_enable_back_forward_navigation_gestures(false);

        let content = UserContentManager::new();
        let view = WebView::builder()
            .settings(&settings)
            .user_content_manager(&content)
            .hexpand(true)
            .vexpand(true)
            .build();

        let chat = ChatView {
            inner: Rc::new(RefCell::new(Inner {
                view: view.clone(),
                readiness: Readiness::Loading(Vec::new()),
            })),
        };

        for &name in HANDLERS {
            let weak = Rc::downgrade(&chat.inner);
            content.connect_script_message_received(Some(name), move |_, value| {
                if let Some(inner) = weak.upgrade() {
                    answer_promise(&inner.borrow().view, value);
                }
            });
            content.register_script_message_handler(name, None);
        }

        let weak = Rc::downgrade(&chat.inner);
        let channel_name = channel.to_owned();
        let topic = topic.to_owned();
        view.connect_load_changed(move |_, event| {
            if event != LoadEvent::Finished {
                return;
            }
            if let Some(inner) = weak.upgrade() {
                ChatView { inner }.finish_loading(&channel_name, &topic);
            }
        });

        let page = write_page(
            pages,
            &style.initial_document(channel, &default_font(), &cache_token()),
        );
        match page {
            Ok(path) => view.load_uri(&glib::filename_to_uri(&path, None).expect("absolute path")),
            Err(e) => eprintln!("textual-gtk: cannot write the view page: {e}"),
        }
        chat
    }

    pub fn widget(&self) -> WebView {
        self.inner.borrow().view.clone()
    }

    /// Appends one rendered line (see `Line::render`).
    pub fn append(&self, html: String, line_id: &str) {
        let script = format!(
            "MessageBuffer.bufferElementAppend({}, [{}]);",
            serde_json::Value::String(html),
            serde_json::Value::String(line_id.to_owned())
        );
        self.run_when_ready(script);
    }

    /// Shows `topic` (IRC formatting allowed) in the style's topic bar.
    pub fn set_topic(&self, topic: &str, style: &Style) {
        let fragment = |ctx: &crate::style::template::Value| {
            style
                .render("formattedMessageFragment", ctx)
                .unwrap_or_default()
        };
        let html = crate::style::body::render(topic, false, &fragment);
        let plain: String = topic.chars().filter(|c| *c >= ' ').collect();
        let json = |s: String| serde_json::Value::String(s).to_string();
        let script = format!("Textual.setTopicBarValue({}, {});", json(plain), json(html));
        self.run_when_ready(script);
    }

    /// Runs `expression` in the page and hands its JSON result to `done`.
    pub fn query(&self, expression: &str, done: impl FnOnce(Option<String>) + 'static) {
        self.inner.borrow().view.evaluate_javascript(
            expression,
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            move |result| done(result.ok().and_then(|v| v.to_json(0)).map(Into::into)),
        );
    }

    /// Textual drops calls made before the page finishes loading, so they
    /// wait until then.
    fn run_when_ready(&self, script: String) {
        let mut inner = self.inner.borrow_mut();
        match &mut inner.readiness {
            Readiness::Loading(queue) => queue.push(script),
            Readiness::Ready => evaluate(&inner.view, &script),
        }
    }

    /// Textual's `logViewWebViewFinishedLoading`: the calls that set up
    /// auto-scroll and the topic, after which lines may be appended.
    fn finish_loading(&self, channel: &str, topic: &str) {
        let json = |s: &str| serde_json::Value::String(s.to_owned()).to_string();
        let setup = format!(
            "Textual.viewInitiated(\"channel\", \"client\", \"channel\", {channel});\
             _Textual.viewFinishedLoading({{\"selected\": true, \"visible\": true, \
             \"reloadingTheme\": false, \"textSizeMultiplier\": 1.0, \"scrollbackLimit\": 0}});\
             Textual.setTopicBarValue({topic}, {topic});\
             _Textual.viewFinishedLoadingHistory();",
            channel = json(channel),
            topic = json(topic),
        );
        let mut inner = self.inner.borrow_mut();
        evaluate(&inner.view, &setup);
        if let Readiness::Loading(queue) = std::mem::replace(&mut inner.readiness, Readiness::Ready)
        {
            for script in queue {
                evaluate(&inner.view, &script);
            }
        }
    }
}

fn evaluate(view: &WebView, script: &str) {
    view.evaluate_javascript(
        script,
        None,
        None,
        None::<&gtk::gio::Cancellable>,
        |result| {
            if let Err(e) = result {
                eprintln!("textual-gtk: script failed: {e}");
            }
        },
    );
}

/// `scriptSink.js` sends `{promiseIndex, values}` for calls that want an
/// answer and waits on `appInternal.promiseKept`.
fn answer_promise(view: &WebView, value: &webkit6::javascriptcore::Value) {
    if !value.is_object() || !value.object_has_property("promiseIndex") {
        return;
    }
    let Some(index) = value.object_get_property("promiseIndex") else {
        return;
    };
    let index = index.to_double();
    if index >= 0.0 {
        evaluate(view, &format!("appInternal.promiseKept({index}, null);"));
    }
}

fn default_font() -> Font {
    Font {
        family: "Sans".into(),
        points: 12.0,
    }
}

fn cache_token() -> String {
    format!("{}", glib::monotonic_time())
}

fn write_page(dir: &Path, html: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.html", glib::uuid_string_random()));
    std::fs::write(&path, html)?;
    Ok(path)
}
