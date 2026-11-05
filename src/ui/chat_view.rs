//! A buffer's message view: a WebKitGTK page built from the style's
//! `baseLayout`, driven through Textual's own JavaScript API. It keeps no
//! lines of its own: after a load or a crash it asks the view to refill it
//! from the model's scrollback.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::glib;
use webkit6::prelude::*;
use webkit6::{
    LoadEvent, NavigationPolicyDecision, PolicyDecision, PolicyDecisionType, Settings,
    UserContentManager, WebContext, WebView,
};

use textual_core::style::{Font, Style};

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

/// More web-process crashes than this within the window and the view
/// stops reloading.
const MAX_CRASHES: usize = 3;
const CRASH_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);

struct Inner {
    view: WebView,
    page_uri: String,
    channel: String,
    style: Rc<Style>,
    /// Whether the page has been filled since it last loaded. Calls made
    /// before then are dropped by Textual's JS, and the model holds the
    /// lines anyway, so `append` and `set_topic` wait for `fill`.
    ready: bool,
    /// The page finished loading and wants its lines back.
    needs_replay: bool,
    /// When the web process last ended, to stop reloading a page that
    /// crashes every time.
    crashes: Vec<std::time::Instant>,
}

impl Inner {
    fn append_script((html, id): &(String, String)) -> String {
        format!(
            "MessageBuffer.bufferElementAppend({}, [{}]);",
            json(html),
            json(id)
        )
    }

    fn topic_script(&self, topic: &str) -> String {
        let fragment = |ctx: &textual_core::style::template::Value| {
            self.style
                .render("formattedMessageFragment", ctx)
                .unwrap_or_default()
        };
        // Textual's wording for a channel without a topic (TVCMainWindow[vi3-23]).
        let topic = if topic.is_empty() {
            "(No Topic)"
        } else {
            topic
        };
        let html = textual_core::style::body::render(topic, false, &fragment);
        let plain: String = topic.chars().filter(|c| *c >= ' ').collect();
        format!(
            "Textual.setTopicBarValue({}, {});",
            json(&plain),
            json(&html)
        )
    }

    /// Textual's `logViewWebViewFinishedLoading`: set up auto-scroll and the
    /// view, then wait for the model's lines.
    fn page_ready(&mut self) {
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
        self.needs_replay = true;
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
    /// `related` shares its web process; `on_ready` runs when the page has
    /// loaded and `needs_replay` is set.
    pub fn new(
        style: Rc<Style>,
        channel: &str,
        pages: &Path,
        related: Option<&WebView>,
        on_ready: impl Fn() + 'static,
    ) -> Self {
        let settings = Settings::new();
        settings.set_enable_developer_extras(std::env::var_os("TEXTUAL_GTK_DEVTOOLS").is_some());
        settings.set_enable_back_forward_navigation_gestures(false);

        let content = UserContentManager::new();
        let mut builder = WebView::builder()
            .settings(&settings)
            .user_content_manager(&content)
            .hexpand(true)
            .vexpand(true);
        if let Some(related) = related {
            builder = builder.related_view(related);
        }
        let view = builder.build();

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
                style,
                ready: false,
                needs_replay: false,
                crashes: Vec::new(),
            })),
        };

        let on_ready = Rc::new(on_ready);
        for &name in HANDLERS {
            let weak = Rc::downgrade(&chat.inner);
            let on_ready = on_ready.clone();
            content.connect_script_message_received(Some(name), move |_, value| {
                let Some(inner) = weak.upgrade() else { return };
                if name == "finishedLayingOutView" {
                    inner.borrow_mut().page_ready();
                    on_ready();
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
            let Some(inner) = weak.upgrade() else { return };
            let mut inner = inner.borrow_mut();
            inner.ready = false;
            let now = std::time::Instant::now();
            inner
                .crashes
                .retain(|t| now.duration_since(*t) < CRASH_WINDOW);
            inner.crashes.push(now);
            if inner.crashes.len() > MAX_CRASHES {
                eprintln!(
                    "textual-gtk: the view's web process keeps ending ({reason:?}); giving up"
                );
                return;
            }
            eprintln!("textual-gtk: the view's web process ended ({reason:?}); reloading");
            view.load_uri(&inner.page_uri);
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

    pub fn reload(&self) {
        self.inner.borrow().view.reload();
    }

    pub fn needs_replay(&self) -> bool {
        self.inner.borrow().needs_replay
    }

    /// Puts the model's topic and lines into a freshly loaded page, after
    /// which `append` and `set_topic` reach it directly.
    pub fn fill(&self, topic: &str, lines: impl Iterator<Item = (String, String)>) {
        let mut inner = self.inner.borrow_mut();
        evaluate(&inner.view, &inner.topic_script(topic));
        for line in lines {
            evaluate(&inner.view, &Inner::append_script(&line));
        }
        inner.ready = true;
        inner.needs_replay = false;
    }

    /// Appends one rendered line (see `Line::render`).
    pub fn append(&self, html: String, line_id: &str) {
        let inner = self.inner.borrow();
        if inner.ready {
            evaluate(
                &inner.view,
                &Inner::append_script(&(html, line_id.to_owned())),
            );
        }
    }

    /// Shows `topic` (IRC formatting allowed) in the style's topic bar.
    pub fn set_topic(&self, topic: &str) {
        let inner = self.inner.borrow();
        if inner.ready {
            evaluate(&inner.view, &inner.topic_script(topic));
        }
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

/// The view only ever shows its own page. Any other navigation (a link, a
/// script, a new window) is refused; an http(s) link the user clicked opens
/// in their browser instead.
fn keep_on_page(page_uri: &str, decision: &PolicyDecision, kind: PolicyDecisionType) -> bool {
    if !matches!(
        kind,
        PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction
    ) {
        return false;
    }
    let action = decision
        .downcast_ref::<NavigationPolicyDecision>()
        .and_then(|d| d.navigation_action());
    let uri = action
        .as_ref()
        .and_then(|a| a.clone().request())
        .and_then(|r| r.uri())
        .map(|u| u.to_string())
        .unwrap_or_default();
    if kind == PolicyDecisionType::NavigationAction && uri == page_uri {
        decision.use_();
        return true;
    }
    decision.ignore();
    let clicked = action.is_some_and(|a| a.is_user_gesture());
    if clicked && (uri.starts_with("https://") || uri.starts_with("http://")) {
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
