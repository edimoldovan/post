//! `post --serve` as a real process, on a computer with no screen.
//!
//! The server has no window, so it has to come up and serve with `DISPLAY` and
//! `WAYLAND_DISPLAY` both unset — that is what these tests take away. What it
//! cannot do without is a way to ask the user, and that is a process of its own:
//! a stub script stands in for the `post --confirm` window here, answering
//! down the same pipe a real dialog would, so all four answers can be driven
//! from a test.

#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

const EXE: &str = env!("CARGO_BIN_EXE_post");

/// A dialog that approves, the way a click on Approve does.
const APPROVES: &str = r#"read question
printf '%s\n' '{"decision":"approve","always_allow":false}'"#;

/// A dialog that refuses.
const DENIES: &str = r#"read question
printf '%s\n' '{"decision":"deny","always_allow":false}'"#;

/// A dialog that reads the question and dies without a word — a crash, a kill,
/// a compositor that would not open the window.
const SAYS_NOTHING: &str = "read question\nexit 0";

/// A dialog that keeps the question it was asked, ticks "always allow", and
/// approves — so a test can read what the user would have seen.
const APPROVES_ALWAYS: &str = r#"IFS= read -r question
printf '%s\n' "$question" > "$(dirname "$0")/asked.json"
printf '%s\n' '{"decision":"approve","always_allow":true}'"#;

/// A scratch home for one test. The socket, the allowlist and everything the
/// server writes on startup land under here, nowhere near the real ones.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("post-tests")
        .join(format!("headless-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch dir");
    dir
}

/// Writes the stub dialog this test wants the server to run.
fn dialog(dir: &Path, body: &str) {
    let path = dir.join("dialog.sh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write the stub dialog");
    std::fs::set_permissions(&path, PermissionsExt::from_mode(0o755)).expect("make it runnable");
}

/// A running server, killed when the test ends however it ends.
struct Serving {
    child: Child,
    dir: PathBuf,
    socket: PathBuf,
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Serving {
    /// True while the process is still up.
    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn talk(&self) -> Talk {
        Talk::to(&self.socket)
    }

    /// Where the allowlist ends up under this test's config home.
    fn allowlist(&self) -> PathBuf {
        self.dir.join("post").join("allowlist.toml")
    }
}

/// Starts `post --serve` with no display of any kind, and waits for its
/// socket. The stub dialog from [`dialog`] is what it will ask with.
fn serve(dir: PathBuf) -> Serving {
    let socket = dir.join("post.sock");
    let child = Command::new(EXE)
        .arg("--serve")
        // No screen, at all: if anything in this role reached for a window it
        // would die here instead of serving.
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        // The socket, the daemon it would ask, the allowlist and the assistant
        // configs it registers with: all under the scratch dir.
        .env("XDG_RUNTIME_DIR", &dir)
        .env("XDG_CONFIG_HOME", &dir)
        .env("XDG_DATA_HOME", &dir)
        .env("HOME", &dir)
        .env("POST_CONFIRM_CMD", dir.join("dialog.sh"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("post --serve should start");
    post_app::ipc::wait_for_socket(&socket, Duration::from_secs(20))
        .expect("the headless server should bind its socket");
    Serving { child, dir, socket }
}

/// One MCP client on the server's socket.
struct Talk {
    out: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Talk {
    fn to(socket: &Path) -> Talk {
        let out = UnixStream::connect(socket).expect("a connection to the server");
        out.set_read_timeout(Some(Duration::from_secs(30)))
            .expect("a read timeout, so a hang fails rather than waits");
        let reader = BufReader::new(out.try_clone().expect("clone"));
        Talk { out, reader }
    }

    fn send(&mut self, id: Value, method: &str, params: Value) {
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.out, "{line}").expect("write");
        self.out.flush().expect("flush");
    }

    fn call(&mut self, id: u64, tool: &str, args: Value) {
        self.send(
            json!(id),
            "tools/call",
            json!({"name": tool, "arguments": args}),
        );
    }

    fn reply(&mut self) -> Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("a reply line");
        serde_json::from_str(&line).expect("json")
    }

    /// The text of one tool result.
    fn result(&mut self) -> String {
        let reply = self.reply();
        assert!(reply.get("error").is_none(), "got {reply}");
        reply["result"]["content"][0]["text"]
            .as_str()
            .expect("result text")
            .to_string()
    }
}

/// The handshake, the tool list and a read tool, with no display anywhere.
#[test]
fn the_server_serves_with_no_display() {
    let dir = scratch("no-display");
    dialog(&dir, DENIES);
    let mut serving = serve(dir);
    let mut talk = serving.talk();

    talk.send(json!("init"), "initialize", json!({}));
    let reply = talk.reply();
    assert_eq!(
        reply["result"]["protocolVersion"],
        post_app::mcp::proto::PROTOCOL_VERSION
    );

    talk.send(json!(2), "tools/list", json!({}));
    let reply = talk.reply();
    assert_eq!(
        reply["result"]["tools"].as_array().expect("tools").len(),
        18,
        "the whole surface, served without a screen"
    );

    // A free tool goes straight through, answered by the server itself — and
    // it needed no window to give its answer.
    talk.call(3, "post_accounts", json!({}));
    let text = talk.result();
    assert!(text.contains("accounts"), "got {text:?}");

    assert!(
        serving.running(),
        "a headless server does not exit for want of a display"
    );
}

/// Approve in the dialog, and the call goes on to do its work.
#[test]
fn a_dialog_that_approves_lets_the_call_through() {
    let dir = scratch("approve");
    dialog(&dir, APPROVES);
    let serving = serve(dir);
    let mut talk = serving.talk();

    talk.call(
        1,
        "post_send",
        json!({"email": "ed@acme.com", "draft_id": "r-123"}),
    );
    let text = talk.result();
    assert_ne!(text, post_app::confirm::DENIED);
    // Past the gate and into the mail library: no account is connected under
    // this scratch home, so that is what it says.
    assert!(!text.contains("no such tool"), "got {text:?}");
    assert!(
        !serving.allowlist().exists(),
        "one approval is one approval; nothing should be written down"
    );
}

/// Deny in the dialog, and nothing happens.
#[test]
fn a_dialog_that_denies_stops_the_call() {
    let dir = scratch("deny");
    dialog(&dir, DENIES);
    let serving = serve(dir);
    let mut talk = serving.talk();

    talk.call(
        1,
        "post_send",
        json!({"email": "ed@acme.com", "draft_id": "r-123"}),
    );
    assert_eq!(talk.result(), post_app::confirm::DENIED);
}

/// A dialog that answers nothing is a refusal: the server never guesses yes.
#[test]
fn a_dialog_that_says_nothing_is_a_refusal() {
    let dir = scratch("silent");
    dialog(&dir, SAYS_NOTHING);
    let serving = serve(dir);
    let mut talk = serving.talk();

    talk.call(
        1,
        "post_send",
        json!({"email": "ed@acme.com", "draft_id": "r-123"}),
    );
    assert_eq!(talk.result(), post_app::confirm::DENIED);
}

// ------------------------------------------------------------------- the mail

/// The one mail verb that acts reaches the server and is asked about. The
/// question carries the whole message when the draft can be read from Gmail —
/// the mail library's own tests show that — and says so when it cannot: here
/// there is no account connected, so nothing is approved by seeing a tool name.
#[test]
fn sending_is_asked_about_and_says_so_when_the_draft_cannot_be_read() {
    let dir = scratch("send");
    dialog(&dir, APPROVES_ALWAYS);
    let serving = serve(dir);
    let mut talk = serving.talk();

    talk.call(1, "post_send", json!({"email": "ed@acme.com", "draft_id": "r-123"}));
    let text = talk.result();
    // It got past the gate and into the mail library: there is no account
    // connected under this scratch home, so that is what it says. What it must
    // never say is that the server does not know the verb.
    assert_ne!(text, post_app::confirm::DENIED);
    assert!(!text.contains("no such tool"), "got {text:?}");

    let asked: Value = serde_json::from_str(
        &std::fs::read_to_string(serving.dir.join("asked.json")).expect("the question"),
    )
    .expect("json");
    assert_eq!(asked["tool"], "post_send");
    let sentence = asked["sentence"].as_str().expect("a sentence");
    assert!(sentence.contains("r-123"), "got {sentence}");
    assert!(sentence.contains("could not be read"), "got {sentence}");
    // And no checkbox: a reply can never be allowed for good, so the dialog is
    // not offered the words for one.
    assert_eq!(asked["always_label"], "");

    // The dialog ticked it anyway, and it still was not written down.
    let allowed = serving.allowlist();
    let written = std::fs::read_to_string(&allowed).unwrap_or_default();
    assert!(
        !written.contains("post_send"),
        "sending mail was allowed for good: {written}"
    );
}

/// Deny, and nothing leaves.
#[test]
fn a_denied_reply_is_not_sent() {
    let dir = scratch("send-deny");
    dialog(&dir, DENIES);
    let serving = serve(dir);
    let mut talk = serving.talk();

    talk.call(
        1,
        "post_send",
        json!({"email": "ed@acme.com", "draft_id": "r-123"}),
    );
    assert_eq!(talk.result(), post_app::confirm::DENIED);
}

/// And the verbs that only read are not asked about at all: a dialog that
/// refuses everything never sees them.
#[test]
fn the_free_mail_verbs_are_never_asked_about() {
    let dir = scratch("mail-free");
    dialog(&dir, DENIES);
    let serving = serve(dir);
    let mut talk = serving.talk();

    talk.call(1, "post_accounts", json!({}));
    let text = talk.result();
    assert_ne!(text, post_app::confirm::DENIED);
    assert!(text.contains("accounts"), "an answer, not a refusal: got {text:?}");
}
