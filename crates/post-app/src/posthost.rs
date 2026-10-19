//! One Post window, and where a second `post …` goes.
//!
//! Post's window holds no document and answers no tool call — the mail verbs
//! are files on disk, answered wherever they are asked — so there is nothing
//! for the server to forward here. What is left is the part that matters to a
//! person typing commands: `post list ed@…` twice in a row must be one window
//! that navigates, not two windows fighting over a mailbox.
//!
//! So Post keeps its own door beside the app socket. Whoever binds it is the
//! window; whoever cannot sends the view down it and exits, and the window that
//! is already up navigates and comes to the front.
//!
//! [`open`] is that errand from a process that will never be the window — the
//! assistant's shim answering `post_show`, and `post list …` in a
//! terminal, which owes the prompt back immediately. It hands the view over the
//! same way, and when nothing answers it starts a detached window process
//! rather than becoming one.
//!
//! [`start`] is the other side of that: the detached process reading its own
//! role back, claiming the socket and holding the event loop.

use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use serde_json::json;

use crate::ipc::{self, Msg};
use post_ui::Route;

/// What the Post window calls itself to the desktop: its Wayland app id, and
/// the `argv[0]` X11 turns into WM_CLASS. `packaging/post.desktop` carries
/// the same string as `StartupWMClass`, which is what groups the window under
/// the Post icon.
pub const APP_ID: &str = "post";

/// What a launch decided to do.
pub enum Outcome {
    /// A window was already there; it took the view. This process is done.
    HandedOff,
    /// Show the window. Views handed over later arrive on this channel.
    Run(Receiver<Route>),
}

/// Set once the window has a repaint handle, so a view arriving on the socket
/// wakes an idle window instead of waiting for the mouse.
static WAKE: OnceLock<Mutex<Option<eframe::egui::Context>>> = OnceLock::new();

/// Gives this module a handle it can use to wake the window.
pub fn set_wake(ctx: eframe::egui::Context) {
    match WAKE.get() {
        Some(slot) => {
            if let Ok(mut guard) = slot.lock() {
                *guard = Some(ctx);
            }
        }
        None => {
            let _ = WAKE.set(Mutex::new(Some(ctx)));
        }
    }
}

fn wake() {
    if let Some(slot) = WAKE.get()
        && let Ok(guard) = slot.lock()
        && let Some(ctx) = guard.as_ref()
    {
        ctx.request_repaint();
    }
}

/// Post's own socket, beside the app socket and named after it, so it is one
/// per user wherever that one ends up.
pub fn socket_path() -> PathBuf {
    let app = crate::mcp::proto::socket_path();
    let name = app
        .file_stem()
        .map(|stem| format!("{}-window.sock", stem.to_string_lossy()))
        .unwrap_or_else(|| "post-window.sock".to_string());
    match app.parent() {
        Some(dir) => dir.join(name),
        None => PathBuf::from(name),
    }
}

/// Decides what this launch is: the window, or the messenger.
pub fn start(view: &Route) -> Outcome {
    start_at(&socket_path(), view)
}

/// The same decision on a named socket, which is how the tests drive it with
/// no window anywhere.
pub fn start_at(path: &Path, view: &Route) -> Outcome {
    if hand_off(path, view) {
        return Outcome::HandedOff;
    }
    let Some(listener) = claim(path) else {
        // The socket went to someone else between the two tries — that is a
        // window, so give it the view rather than opening a second one.
        if hand_off(path, view) {
            return Outcome::HandedOff;
        }
        // Nothing answers and the socket cannot be had. Better a window with no
        // door than no window at all; a later command opens its own.
        eprintln!("post: {} is not usable — this window is on its own", path.display());
        return Outcome::Run(channel().1);
    };
    let (tx, rx) = channel();
    serve(listener, tx);
    Outcome::Run(rx)
}

/// Shows a view from a process that will not itself be the window — the MCP
/// shim, which has stdin and stdout to keep answering on, and `post list …`
/// in a terminal, which owes the prompt back at once.
///
/// The two moves are the ones [`start`] already makes, minus the third: hand
/// the view to the window that is up, and otherwise start the window process,
/// which comes up at that view through [`crate::role`] like any other launch.
/// Nothing here runs a mail verb; the window is a role, not a command line.
///
/// The window that starts is detached — its own session, no stdio — so it
/// belongs to the desktop rather than to the terminal that asked for it.
pub fn open(view: &Route) -> Result<(), String> {
    open_at(&socket_path(), view, |args| {
        crate::ipc::spawn_self_named(Some(APP_ID), args)
            .map_err(|e| format!("could not start the Post window: {e}"))
    })
}

/// The same on a named socket, with the way to start a window passed in —
/// which is how the tests drive both halves of the decision with no window
/// anywhere.
pub fn open_at(
    path: &Path,
    view: &Route,
    spawn: impl FnOnce(&[String]) -> Result<(), String>,
) -> Result<(), String> {
    // Asked before either move, so a route a window could take but a launch
    // could not name means the same thing whether or not one is up.
    if Route::parse(&view.to_string()).as_ref() != Some(view) {
        return Err(format!("\"{view}\" is not a route Post can open"));
    }
    if hand_off(path, view) {
        return Ok(());
    }
    spawn(&launch_args(view))
}

/// The command line that starts a window at a route: the word that means
/// "be the window", then the route.
pub fn launch_args(route: &Route) -> Vec<String> {
    vec![crate::role::WINDOW_ARG.to_string(), route.to_string()]
}

/// Gives up the socket. Called when the window closes, so the next launch does
/// not talk to a door nobody is behind.
pub fn release() {
    let _ = std::fs::remove_file(socket_path());
}

/// Takes the socket, which is what makes this process *the* Post window.
///
/// A file left behind by a crash answers nothing, so it is replaced — but only
/// after asking twice, because the one thing that must never happen is
/// unlinking a live window's socket and opening a second window beside it.
fn claim(path: &Path) -> Option<UnixListener> {
    if UnixStream::connect(path).is_ok() {
        return None;
    }
    if let Ok(listener) = UnixListener::bind(path) {
        return Some(listener);
    }
    if UnixStream::connect(path).is_ok() {
        return None;
    }
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path).ok()
}

/// Asks the window that holds the socket to show this view. False when nothing
/// is there to ask.
fn hand_off(path: &Path, view: &Route) -> bool {
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    let msg = Msg::View {
        id: Some(1),
        route: view.to_string(),
    };
    if ipc::send(&mut stream, &msg).is_err() {
        return false;
    }
    let mut reader = BufReader::new(stream);
    matches!(
        ipc::recv(&mut reader),
        Ok(Some(Msg::Reply { result: Ok(_), .. }))
    )
}

/// Answers the socket until the window goes away.
fn serve(listener: UnixListener, tx: Sender<Route>) {
    std::thread::Builder::new()
        .name("post-host".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let _ = answer(stream, &tx);
                });
            }
        })
        .ok();
}

/// One connection: every view it asks for, acknowledged as it lands.
fn answer(stream: UnixStream, tx: &Sender<Route>) -> std::io::Result<()> {
    let mut out = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    loop {
        let Some(msg) = ipc::recv(&mut reader)? else {
            return Ok(());
        };
        let (id, result) = match msg {
            Msg::View { id, route } => {
                let result = match Route::parse(&route) {
                    None => Err(format!("\"{route}\" is not a route Post can open")),
                    Some(route) => match tx.send(route) {
                        Ok(()) => Ok(json!("here")),
                        Err(_) => Err("the Post window closed".to_string()),
                    },
                };
                wake();
                (id, result)
            }
            other => (
                request_id(&other),
                Err("not something the Post window answers".to_string()),
            ),
        };
        ipc::send(&mut out, &Msg::Reply { id, result })?;
    }
}

fn request_id(msg: &Msg) -> Option<u64> {
    match msg {
        Msg::View { id, .. } | Msg::Reply { id, .. } => *id,
    }
}
