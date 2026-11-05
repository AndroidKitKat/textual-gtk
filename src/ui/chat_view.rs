//! A channel's message view: a WebKitGTK page built from the style's
//! `baseLayout`, driven through Textual's own JavaScript API.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::glib;
use webkit6::prelude::*;
use webkit6::{
    LoadEvent, NavigationPolicyDecision, PolicyDecision, PolicyDecisionType, Settings,
    UserContentManager, WebContext, WebView,
};

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

/// Textual's hard scrollback limit (`messageBuffer.js`). The view keeps
/// this many lines so it can rebuild the page after a reload or a crash.
const KEPT_LINES: usize = 1000;

struct Inner {
    view: WebView,
    page_uri: String,
    channel: String,
    /// Whether the current page has finished Textual's load handshake.
    /// Calls made before then are dropped by Textual's JS, so they wait.
    ready: bool,
    lines: VecDeque<(String, String)>,
    topic: Option<(String, String)>,
}

impl Inner {
    fn append_script((html, id): &(String, String)) -> String {
        format!(
            "MessageBuffer.bufferElementAppend({}, [{}]);",
            json(html),
            json(id)
        )
    }

    fn topic_script((plain, html): &(String, String)) -> String {
        format!("Textual.setTopicBarValue({}, {});", json(plain), json(html))
    }

    /// Textual's `logViewWebViewFinishedLoading`: set up auto-scroll and the
    /// view, then put back everything the page should show.
    fn page_ready(&mut self) {
        self.ready = true;
        evaluate(
            &self.view,
            &format!(
                "Textual.viewInitiated(\"channel\", \"client\", \"channel\", {});\
                 _Textual.viewFinishedLoading({{\"selected\": true, \"visible\": true, \
                 \"reloadingTheme\": false, \"textSizeMultiplier\": 1.0, \"scrollbackLimit\": 0}});\
                 _Textual.viewFinishedLoadingHistory();",
                json(&self.channel)
            ),
        );
        if let Some(topic) = &self.topic {
            evaluate(&self.view, &Self::topic_script(topic));
        }
        for line in &self.lines {
            evaluate(&self.view, &Self::append_script(line));
        }
    }
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
    pub fn new(style: &Style, channel: &str, pages: &Path) -> Self {
        let settings = Settings::new();
        settings.set_enable_developer_extras(std::env::var_os("TEXTUAL_GTK_DEVTOOLS").is_some());
        settings.set_enable_back_forward_navigation_gestures(false);

        let content = UserContentManager::new();
        let view = WebView::builder()
            .settings(&settings)
            .user_content_manager(&content)
            .hexpand(true)
            .vexpand(true)
            .build();

        let page_uri = match write_page(
            pages,
            &style.initial_document(channel, &default_font(), &cache_token()),
        ) {
            Ok(path) => glib::filename_to_uri(&path, None)
                .expect("absolute path")
                .to_string(),
            Err(e) => {
                eprintln!("textual-gtk: cannot write the view page: {e}");
                String::new()
            }
        };

        let chat = ChatView {
            inner: Rc::new(RefCell::new(Inner {
                view: view.clone(),
                page_uri: page_uri.clone(),
                channel: channel.to_owned(),
                ready: false,
                lines: VecDeque::new(),
                topic: None,
            })),
        };

        for &name in HANDLERS {
            let weak = Rc::downgrade(&chat.inner);
            content.connect_script_message_received(Some(name), move |_, value| {
                let Some(inner) = weak.upgrade() else { return };
                if name == "finishedLayingOutView" {
                    inner.borrow_mut().page_ready();
                }
                answer_promise(&inner.borrow().view, value);
            });
            content.register_script_message_handler(name, None);
        }

        let weak = Rc::downgrade(&chat.inner);
        view.connect_decide_policy(move |_, decision, kind| {
            let Some(inner) = weak.upgrade() else {
                return false;
            };
            keep_on_page(&inner.borrow().page_uri, decision, kind)
        });

        let weak = Rc::downgrade(&chat.inner);
        view.connect_web_process_terminated(move |view, reason| {
            eprintln!("textual-gtk: the view's web process ended ({reason:?}); reloading");
            if let Some(inner) = weak.upgrade() {
                let mut inner = inner.borrow_mut();
                inner.ready = false;
                view.load_uri(&inner.page_uri);
            }
        });
        view.connect_load_changed({
            let weak = Rc::downgrade(&chat.inner);
            move |_, event| {
                if event == LoadEvent::Started
                    && let Some(inner) = weak.upgrade()
                {
                    inner.borrow_mut().ready = false;
                }
            }
        });

        if !page_uri.is_empty() {
            view.load_uri(&page_uri);
        }
        chat
    }

    pub fn widget(&self) -> WebView {
        self.inner.borrow().view.clone()
    }

    /// Appends one rendered line (see `Line::render`).
    pub fn append(&self, html: String, line_id: &str) {
        let mut inner = self.inner.borrow_mut();
        let line = (html, line_id.to_owned());
        if inner.ready {
            evaluate(&inner.view, &Inner::append_script(&line));
        }
        inner.lines.push_back(line);
        if inner.lines.len() > KEPT_LINES {
            inner.lines.pop_front();
        }
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
        let mut inner = self.inner.borrow_mut();
        let topic = (plain, html);
        if inner.ready {
            evaluate(&inner.view, &Inner::topic_script(&topic));
        }
        inner.topic = Some(topic);
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
}

/// The view only ever shows its own page. Any other navigation (a clicked
/// link, a script, a new window) is refused, and http(s) links open in the
/// user's browser instead.
fn keep_on_page(page_uri: &str, decision: &PolicyDecision, kind: PolicyDecisionType) -> bool {
    if !matches!(
        kind,
        PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction
    ) {
        return false;
    }
    let uri = decision
        .downcast_ref::<NavigationPolicyDecision>()
        .and_then(|d| d.navigation_action())
        .and_then(|a| a.request())
        .and_then(|r| r.uri())
        .map(|u| u.to_string())
        .unwrap_or_default();
    if kind == PolicyDecisionType::NavigationAction && uri == page_uri {
        decision.use_();
        return true;
    }
    decision.ignore();
    if uri.starts_with("https://") || uri.starts_with("http://") {
        gtk::UriLauncher::new(&uri).launch(
            None::<&gtk::Window>,
            None::<&gtk::gio::Cancellable>,
            |_| {},
        );
    }
    true
}

fn json(s: &str) -> String {
    serde_json::Value::String(s.to_owned()).to_string()
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
