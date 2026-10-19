//! The app sockets' own messages, and how a process reaches the server.
//!
//! JSON-RPC lines are MCP and belong to [`crate::mcp::proto`]; a line carrying
//! an `"op"` key is one of these, the traffic to and from the Post window's
//! own socket:
//!
//! ```text
//! view    {"op":"view","id":5,"route":"/ed@…/INBOX"}  put this Post route on screen
//! reply   {"op":"reply","id":5,"ok":true,…}           one per request, by id
//! ```
//!
//! A second `post list ed@…` sends a `view` down the window's socket and
//! exits.

use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// How long a process waits for a freshly spawned server to bind the socket.
pub const SERVER_WAIT: Duration = Duration::from_secs(20);

/// One message on a socket.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// Show this Post route.
    View { id: Option<u64>, route: String },
    /// The answer to one request, matched by id.
    Reply {
        id: Option<u64>,
        result: Result<Value, String>,
    },
}

impl Msg {
    /// The single line this message travels as.
    pub fn encode(&self) -> String {
        let value = match self {
            Msg::View { id, route } => {
                let mut v = json!({"op": "view", "route": route});
                if let Some(id) = id {
                    v["id"] = json!(id);
                }
                v
            }
            Msg::Reply { id, result } => {
                let mut v = match result {
                    Ok(result) => json!({"op": "reply", "ok": true, "result": result}),
                    Err(error) => json!({"op": "reply", "ok": false, "error": error}),
                };
                if let Some(id) = id {
                    v["id"] = json!(id);
                }
                v
            }
        };
        value.to_string()
    }

    /// Reads one back. `None` for anything that is not an op line — an MCP
    /// message, or a line from a newer version than this.
    pub fn parse(line: &str) -> Option<Msg> {
        let value: Value = serde_json::from_str(line).ok()?;
        let op = value.get("op")?.as_str()?;
        let id = value.get("id").and_then(Value::as_u64);
        match op {
            "view" => Some(Msg::View {
                id,
                route: text(&value, "route")?,
            }),
            "reply" => {
                let result = if value.get("ok").and_then(Value::as_bool) == Some(true) {
                    Ok(value.get("result").cloned().unwrap_or(Value::Null))
                } else {
                    Err(value
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("no reason given")
                        .to_string())
                };
                Some(Msg::Reply { id, result })
            }
            _ => None,
        }
    }
}

/// One string field of a message, absent when it is empty or not text.
fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// True when a line is an op message rather than MCP.
pub fn is_op(line: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("op").and_then(Value::as_str).map(str::to_owned))
        .is_some()
}

/// Sends one message and flushes it, so the peer sees it now.
pub fn send(stream: &mut impl Write, msg: &Msg) -> std::io::Result<()> {
    writeln!(stream, "{}", msg.encode())?;
    stream.flush()
}

/// Reads lines until one is a message. `Ok(None)` means the peer hung up.
pub fn recv(reader: &mut impl BufRead) -> std::io::Result<Option<Msg>> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if let Some(msg) = Msg::parse(&line) {
            return Ok(Some(msg));
        }
    }
}

/// A connection to the server, starting one if nothing is listening. The
/// stdio shim goes through here before it forwards a call.
pub fn ensure_server() -> std::io::Result<UnixStream> {
    ensure_server_at(&crate::mcp::proto::socket_path(), spawn_server, SERVER_WAIT)
}

/// The same, with the socket, the way to start a server, and the patience all
/// passed in — which is how the tests drive it without a window anywhere.
pub fn ensure_server_at(
    path: &Path,
    spawn: impl FnOnce(),
    wait: Duration,
) -> std::io::Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(path) {
        return Ok(stream);
    }
    spawn();
    wait_for_socket(path, wait)
}

/// Polls until something answers on the socket.
pub fn wait_for_socket(path: &Path, wait: Duration) -> std::io::Result<UnixStream> {
    let deadline = Instant::now() + wait;
    loop {
        if let Ok(stream) = UnixStream::connect(path) {
            return Ok(stream);
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::other(format!(
                "the post server did not start within {} s",
                wait.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Starts `post --serve`: the process that owns the socket and the
/// confirmation broker. It has no window of its own.
pub fn spawn_server() {
    let _ = spawn_self(&["--serve"]);
}

/// Starts another copy of this executable under the arguments that ask for a
/// role — the one way a process with no window of its own puts one on screen.
/// The arguments are a role, never a shell command line.
///
/// What starts here outlives whoever asked for it: a server, or a window
/// someone will still be reading from tomorrow. So it is detached — no stdio,
/// and a session of its own, which is what keeps the terminal's hangup from
/// taking the window down with it when the terminal closes.
pub fn spawn_self<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> std::io::Result<()> {
    spawn_self_named(None, args)
}

/// The same, under a name the new process calls itself by. That name is
/// `argv[0]`, which on X11 is the window's WM_CLASS — so it is what a desktop
/// entry's `StartupWMClass` has to match for the launcher to group the window
/// under the right icon. `None` keeps the executable's own name.
pub fn spawn_self_named<S: AsRef<std::ffi::OsStr>>(
    name: Option<&str>,
    args: &[S],
) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("post"));
    let mut command = std::process::Command::new(exe);
    if let Some(name) = name {
        command.arg0(name);
    }
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: between fork and exec only async-signal-safe calls are allowed.
    // setsid() is one syscall and nothing else runs here.
    unsafe {
        command.pre_exec(|| {
            new_session();
            Ok(())
        })
    };
    command.spawn().map(|_| ())
}

/// Leaves the caller's session and terminal behind. Failing means this process
/// already leads a session, which is the same place we wanted to be.
///
/// One libc call, not worth a dependency.
fn new_session() {
    unsafe extern "C" {
        fn setsid() -> i32;
    }
    unsafe { setsid() };
}
