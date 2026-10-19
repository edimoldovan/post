//! The server, driven over socket pairs with no screen anywhere.
//!
//! Everything the server does apart from drawing the confirm window happens
//! here: the handshake, a tool that only looks running at once, and the gate
//! an acting tool waits at.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use post_app::confirm::{Broker, Decision};
use post_app::serve::Server;
use serde_json::{json, Value};

/// A server with an empty allowlist, so every acting tool really does ask.
fn server(name: &str) -> Arc<Server> {
    let dir = std::env::temp_dir()
        .join("post-tests")
        .join(format!("serve-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch dir");
    let file = dir.join("allowlist.toml");
    let _ = std::fs::remove_file(&file);
    Arc::new(Server::new(Broker::new(file)))
}

/// A connection into the server, served on its own thread as a real one is.
fn connect(server: &Arc<Server>) -> UnixStream {
    let (mine, theirs) = UnixStream::pair().expect("a socket pair");
    let server = server.clone();
    std::thread::spawn(move || {
        let _ = post_app::serve::serve_connection(&server, theirs);
    });
    mine
}

fn rpc(stream: &mut UnixStream, id: u64, tool: &str, args: Value) {
    let line = json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": tool, "arguments": args}
    });
    writeln!(stream, "{line}").expect("write");
    stream.flush().expect("flush");
}

fn reply_line(reader: &mut BufReader<UnixStream>) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).expect("a reply line");
    serde_json::from_str(&line).expect("json")
}

/// Waits for the broker to have a question to ask.
fn question(server: &Arc<Server>) -> post_app::confirm::Waiting {
    for _ in 0..400 {
        if let Some(w) = server.broker.front() {
            return w;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("nothing was ever asked about");
}

/// A tool that only looks runs at once: nothing is asked, and its answer —
/// here, what is missing from the call — comes straight back.
#[test]
fn a_tool_that_looks_is_not_asked_about() {
    let server = server("look");
    let mut client = connect(&server);
    let mut reader = BufReader::new(client.try_clone().expect("clone"));

    rpc(&mut client, 2, "post_read", json!({}));
    let reply = reply_line(&mut reader);
    assert_eq!(reply["id"], 2);
    assert_eq!(reply["result"]["isError"], true);
    assert_eq!(server.broker.waiting(), 0, "nothing was asked");
}

/// Denied means nothing happens, and the model is told plainly.
#[test]
fn a_denied_tool_never_runs() {
    let server = server("deny");
    let mut client = connect(&server);
    let mut reader = BufReader::new(client.try_clone().expect("clone"));

    rpc(&mut client, 3, "post_send", json!({"email": "ed@acme.com", "draft_id": "r-1"}));

    let w = question(&server);
    assert_eq!(w.tool, "post_send");
    server.broker.resolve(w.id, Decision::Deny, false);

    let reply = reply_line(&mut reader);
    assert_eq!(reply["id"], 3);
    assert_eq!(reply["result"]["isError"], true);
    assert_eq!(reply["result"]["content"][0]["text"], post_app::confirm::DENIED);
}

/// Two calls in flight at once: the second question does not wait for the
/// first one's answer, and each answer comes back under its own request id.
#[test]
fn calls_queue_up_and_each_answer_finds_its_call() {
    let server = server("queue");
    let mut client = connect(&server);
    let mut reader = BufReader::new(client.try_clone().expect("clone"));

    rpc(&mut client, 10, "post_draft_delete", json!({"email": "ed@acme.com", "draft_id": "r-1"}));
    rpc(&mut client, 11, "post_draft_delete", json!({"email": "ed@acme.com", "draft_id": "r-2"}));
    for _ in 0..400 {
        if server.broker.waiting() == 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(server.broker.waiting(), 2, "both should be asked about");

    for _ in 0..2 {
        let w = question(&server);
        server.broker.resolve(w.id, Decision::Deny, false);
    }
    let mut ids: Vec<u64> = (0..2)
        .map(|_| reply_line(&mut reader)["id"].as_u64().expect("an id"))
        .collect();
    ids.sort();
    assert_eq!(ids, vec![10, 11]);
}

/// The handshake and the tool list are the same over the socket as they are in
/// the shim: a client cannot tell which one it is talking to.
#[test]
fn the_server_answers_the_handshake_itself() {
    let server = server("handshake");
    let mut client = connect(&server);
    let mut reader = BufReader::new(client.try_clone().expect("clone"));

    writeln!(
        client,
        "{}",
        json!({"jsonrpc": "2.0", "id": "init", "method": "initialize", "params": {}})
    )
    .expect("write");
    let reply = reply_line(&mut reader);
    assert_eq!(
        reply["result"]["protocolVersion"],
        post_app::mcp::proto::PROTOCOL_VERSION
    );
    assert_eq!(reply["result"]["serverInfo"]["name"], "post");

    writeln!(
        client,
        "{}",
        json!({"jsonrpc": "2.0", "id": 9, "method": "tools/list"})
    )
    .expect("write");
    let reply = reply_line(&mut reader);
    assert_eq!(
        reply["result"]["tools"].as_array().expect("tools").len(),
        post_app::mcp::proto::definitions().len()
    );
}
