//! `post --serve`: the one process that is always there while an assistant
//! uses Post.
//!
//! It owns the app socket — binding it *is* the single-instance lock — and the
//! one thing that must not be duplicated: the confirmation broker. Every tool
//! call that acts comes here, so it can be asked about before it runs.
//!
//! It is headless: no event loop, no viewport, no window, ever. The one thing
//! it has to put on screen — the question before something acts — is a
//! process of its own, `post --confirm`, one per question. A window is a whole
//! winit event loop for the life of the process, and the server would be
//! paying for it around the clock to show a dialog for a few seconds a day —
//! on Wayland paying with an empty window it has no way to unmap.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::confirm::Broker;
use crate::mcp::proto;

/// The server's shared state.
pub struct Server {
    pub broker: Arc<Broker>,
}

impl Server {
    pub fn new(broker: Broker) -> Server {
        Server {
            broker: Arc::new(broker),
        }
    }

    /// Runs one tool. Every call goes through here, so this is where the
    /// confirmation gate sits — before Post acts.
    pub fn call_tool(&self, name: &str, args: Value) -> Result<Value, String> {
        self.broker.decide(name, &args)?;
        if name.starts_with(crate::mcp::tools::POST_PREFIX) {
            return crate::mcp::tools::post_call(name, &args);
        }
        Err(format!("no such tool: {name}"))
    }
}

/// Binds the socket and serves until the process ends.
///
/// If another server already answers, this one has nothing to do and says so
/// by exiting quietly: whoever started it only wanted *a* server running.
pub fn run() {
    let path = proto::socket_path();
    let Some(listener) = proto::bind_socket(&path) else {
        return;
    };
    let server = Arc::new(Server::new(Broker::new(crate::confirm::allowlist_path())));
    // The thread that asks the questions: it starts a dialog process for each
    // one and waits for its answer, so nothing on this socket can approve.
    {
        let broker = server.broker.clone();
        std::thread::Builder::new()
            .name("post-confirm".into())
            .spawn(move || crate::confirm::serve_dialogs(&broker))
            .ok();
    }
    accept_loop(&server, listener);
    let _ = std::fs::remove_file(&path);
}

fn accept_loop(server: &Arc<Server>, listener: UnixListener) {
    for stream in listener.incoming().flatten() {
        let server = server.clone();
        std::thread::spawn(move || {
            if let Err(e) = serve_connection(&server, stream) {
                eprintln!("post: connection ended: {e}");
            }
        });
    }
}

/// One connection: MCP, one message per thread. A call can sit in the confirm
/// queue for two minutes; the next message on the same connection must not
/// wait behind it. Replies carry their request's id, so order is not ours to
/// keep.
///
/// Public because the tests drive a real server through it over a socket pair.
pub fn serve_connection(server: &Arc<Server>, stream: UnixStream) -> std::io::Result<()> {
    let out = Arc::new(Mutex::new(stream.try_clone()?));
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let server = server.clone();
        let out = out.clone();
        let message = line.clone();
        std::thread::spawn(move || {
            let dispatch = move |name: &str, args: Value| server.call_tool(name, args);
            if let Some(response) = proto::handle_message_with(&message, &dispatch) {
                if let Ok(mut out) = out.lock() {
                    let _ = writeln!(out, "{response}");
                    let _ = out.flush();
                }
            }
        });
    }
}
