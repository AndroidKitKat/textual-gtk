//! The one loop. Everything that happens to the client arrives here as a
//! `Request`: a model `Input` from a widget or a socket, the window
//! closing, or the probe's tick. Each pass updates the `Workspace`, carries
//! out its effects and patches the view.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use adw::prelude::*;
use textual_core::model::Now;
use textual_core::model::network::{self, Link};
use textual_core::model::workspace::{Input, Out, UiAction, Workspace};
use tokio::sync::mpsc;

use crate::io::Io;
use crate::ui::probe;
use crate::ui::view::View;

/// Messages queued behind the first are taken in the same pass, up to
/// this many, so a NAMES burst costs one render.
const DRAIN: usize = 256;
/// How long a closing window waits for every network to go offline.
const QUIT_WAIT: Duration = Duration::from_secs(3);
const QUIT_REASON: &str = "Textual GTK";

pub type Inbox = mpsc::UnboundedSender<Request>;

#[derive(Debug)]
pub enum Request {
    Input(Input),
    /// The window was closed: quit every network, then exit.
    Quit,
    Probe(probe::Tick),
}

impl From<Input> for Request {
    fn from(input: Input) -> Self {
        Request::Input(input)
    }
}

impl From<UiAction> for Request {
    fn from(action: UiAction) -> Self {
        Request::Input(Input::Ui(action))
    }
}

/// Runs until the window is destroyed. `probe` is the prefix the probe
/// writes to, when it is on.
pub async fn run(
    mut ws: Workspace,
    mut view: View,
    mut io: Io,
    mut rx: mpsc::UnboundedReceiver<Request>,
    probe: Option<PathBuf>,
) {
    let mut quit_at: Option<Instant> = None;
    loop {
        let deadline = [ws.next_wake(), quit_at].into_iter().flatten().min();
        let first = tokio::select! {
            biased;
            request = rx.recv() => match request {
                Some(request) => request,
                None => return,
            },
            () = sleep_until(deadline) => Request::Input(Input::Wake),
        };
        let now = Now {
            mono: Instant::now(),
            wall: chrono::Local::now(),
        };
        let mut out = Out::default();
        let mut tick = None;
        let mut request = Some(first);
        for _ in 0..DRAIN {
            let Some(r) = request.take() else { break };
            match r {
                Request::Input(input) => ws.update(input, now, &mut out),
                Request::Quit if quit_at.is_none() => {
                    quit_at = Some(now.mono + QUIT_WAIT);
                    let servers: Vec<_> = ws.networks().iter().map(|n| n.id()).collect();
                    for server in servers {
                        let input = Input::Network {
                            server,
                            input: network::Input::Disconnect {
                                reason: Some(QUIT_REASON.to_owned()),
                            },
                        };
                        ws.update(input, now, &mut out);
                    }
                }
                Request::Quit => {}
                Request::Probe(t) => {
                    for action in &t.actions {
                        match probe::input_for(&ws, action) {
                            Ok(input) => ws.update(input, now, &mut out),
                            Err(e) => eprintln!("textual-gtk: probe action skipped: {e}"),
                        }
                    }
                    tick = Some(t);
                }
            }
            request = rx.try_recv().ok();
        }
        io.execute(out.effects);
        view.apply(&ws, &out.changes);
        if let Some(tick) = tick {
            if tick.reload
                && let Some(chat) = view.focused_chat()
            {
                chat.reload();
            }
            if tick.close {
                view.window().close();
            }
            if let Some(prefix) = &probe {
                probe::write(prefix, &ws, &view);
            }
        }
        if let Some(at) = quit_at {
            let all_offline = ws
                .networks()
                .iter()
                .all(|n| matches!(n.link(), Link::Offline { .. }));
            if all_offline || now.mono >= at {
                view.window().destroy();
                return;
            }
        }
    }
}

/// Never completes without a deadline, so the loop then waits on the
/// inbox alone.
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}
