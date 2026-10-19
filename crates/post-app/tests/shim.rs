//! `post mcp` end to end, as a client runs it: a real process, over pipes.
//!
//! The point of this one is what does *not* happen. Connecting an assistant,
//! listing the tools and reading mail must not start a server, must not want
//! a screen, and must leave nothing behind.

#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const EXE: &str = env!("CARGO_BIN_EXE_post");

#[test]
fn the_shim_serves_the_handshake_and_mail_without_a_window() {
    let dir = std::env::temp_dir()
        .join("post-tests")
        .join(format!("shim-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch runtime dir");
    let socket = dir.join("post.sock");
    let _ = std::fs::remove_file(&socket);

    let mut child = Command::new(EXE)
        .arg("mcp")
        // A runtime directory of its own: the socket it would use is under
        // here and does not exist. So are the mail accounts: none.
        .env("XDG_RUNTIME_DIR", &dir)
        .env("XDG_CONFIG_HOME", &dir)
        .env("XDG_DATA_HOME", &dir)
        .env("HOME", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("post mcp should start");

    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    for request in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "post_accounts", "arguments": {}}}),
    ] {
        writeln!(stdin, "{request}").expect("write");
    }
    stdin.flush().expect("flush");

    let mut replies = Vec::new();
    for _ in 0..3 {
        let mut line = String::new();
        stdout.read_line(&mut line).expect("a reply");
        replies.push(serde_json::from_str::<Value>(&line).expect("json"));
    }

    assert_eq!(replies[0]["id"], 1);
    assert_eq!(
        replies[0]["result"]["protocolVersion"],
        post_app::mcp::proto::PROTOCOL_VERSION
    );
    assert_eq!(replies[1]["id"], 2);
    assert_eq!(
        replies[1]["result"]["tools"].as_array().expect("tools").len(),
        18,
        "every tool, listed without opening anything"
    );
    // post_accounts is answered by the shim itself: a result, never a protocol
    // error and never a window.
    assert_eq!(replies[2]["id"], 3);
    assert!(replies[2].get("error").is_none(), "got {}", replies[2]);
    let text = replies[2]["result"]["content"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("accounts"), "got {text:?}");

    drop(stdin);
    let status = child.wait().expect("the shim should exit on its own");
    assert!(status.success(), "got {status:?}");

    assert!(
        !socket.exists(),
        "no server should have been started: {} exists",
        socket.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Which lines the shim keeps and which it forwards. Everything read-only is
/// answered in the shim; anything that acts goes to the server, because that is
/// the only process that can ask the user first.
#[test]
fn only_acting_calls_need_the_server() {
    let call = |name: &str| {
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": name, "arguments": {}}})
        .to_string()
    };
    for kept in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}).to_string(),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}).to_string(),
        json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}).to_string(),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        // Mail is a library: the shim runs every free verb itself, so none of
        // them reaches for a server — and neither does the window, which the
        // shim opens down Post's own socket.
        call("post_show"),
        call("post_accounts"),
        call("post_add_account"),
        call("post_list"),
        call("post_read"),
        call("post_mark"),
        call("post_delete"),
        call("post_attachment"),
        // Writing a reply down is free too: nothing has left this computer.
        call("post_draft"),
    ] {
        assert!(
            !post_app::mcp::proto::needs_server(&kept),
            "should be answered in the shim: {kept}"
        );
    }
    for forwarded in [
        // The mail verbs that act go to the server, because the server is the
        // process that can ask the user first.
        call("post_send"),
        call("post_draft_delete"),
        call("post_remove"),
    ] {
        assert!(
            post_app::mcp::proto::needs_server(&forwarded),
            "should go to the server: {forwarded}"
        );
    }
}
