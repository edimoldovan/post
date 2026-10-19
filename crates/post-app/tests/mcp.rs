//! End-to-end MCP test.
//!
//! Speaks newline-delimited JSON-RPC to the protocol layer exactly as a client
//! would. The socket/GUI half is exercised by hand; here we drive the message
//! handling, which is where the logic lives.

use serde_json::{json, Value};

#[test]
fn initialize_reports_the_protocol_and_server() {
    let reply = post_app::mcp::proto::handle_message(
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}).to_string(),
    )
    .expect("a reply");
    assert_eq!(reply["id"], 1);
    assert_eq!(
        reply["result"]["protocolVersion"],
        post_app::mcp::proto::PROTOCOL_VERSION
    );
    assert_eq!(reply["result"]["serverInfo"]["name"], "post");
    assert!(reply["result"]["capabilities"]["tools"].is_object());
}

#[test]
fn tools_list_carries_the_whole_product() {
    let reply = post_app::mcp::proto::handle_message(
        &json!({"jsonrpc":"2.0","id":7,"method":"tools/list"}).to_string(),
    )
    .expect("a reply");
    let listed = reply["result"]["tools"].as_array().expect("tools");
    assert_eq!(listed.len(), post_app::mcp::tools::post_definitions().len());
    assert_eq!(listed.len(), 18, "the whole surface: 17 post verbs + post_show");
    let names: Vec<&str> = listed
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    for name in &names {
        assert!(
            name.starts_with(post_app::mcp::tools::POST_PREFIX),
            "{name} is not named post_verb"
        );
    }
    // Mail: the v1 verbs, served by the shim itself.
    for expected in [
        "post_accounts",
        "post_add_account",
        "post_list",
        "post_read",
        "post_thread",
        "post_mark",
        "post_delete",
        "post_attachment",
        // And the window they are read in.
        "post_show",
    ] {
        assert!(names.contains(&expected), "{expected} is missing");
    }

    // post_show is the one mail tool with a shape of its own: a route, or one
    // of four views, and nothing always needed.
    let show = listed
        .iter()
        .find(|t| t["name"] == "post_show")
        .expect("post_show");
    assert_eq!(show["inputSchema"]["required"], json!([]));
    assert_eq!(
        show["inputSchema"]["properties"]["view"]["enum"],
        json!(["accounts", "inbox", "message", "new"])
    );
    for key in ["route", "account", "id", "draft_id", "to", "cc", "subject", "body", "attach"] {
        assert!(
            show["inputSchema"]["properties"][key].is_object(),
            "{key} is not on post_show"
        );
    }
}

/// `post_show` opens a window, so what is checked here is everything that must
/// happen *before* one does: a call that does not name a view it can open is a
/// tool error saying what is missing, and nothing is started. The calls that
/// would put a window on screen belong to the window's own tests.
#[test]
fn post_show_refuses_what_it_cannot_open() {
    for (arguments, expected) in [
        (json!({}), "view"),
        (json!({"view": "outbox"}), "outbox"),
        (json!({"view": ""}), "is not a view"),
        (json!({"view": "inbox"}), "account"),
        (json!({"view": "message", "account": "ed@acme.com"}), "id"),
        // A route a launch could not name is refused before anything is
        // started, rather than opening some other screen.
        (json!({"view": "inbox", "account": "  "}), "is not a route"),
        (json!({"route": "/ed@acme.com/INBOX/18f3/forward/x"}), "is not a Post route"),
        (json!({"route": "settings"}), "is not a Post route"),
    ] {
        let reply = post_app::mcp::proto::handle_message(
            &json!({"jsonrpc":"2.0","id":31,"method":"tools/call",
                    "params":{"name":"post_show","arguments":arguments.clone()}})
            .to_string(),
        )
        .expect("a reply");
        assert_eq!(reply["result"]["isError"], true, "{arguments}");
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .expect("a reason");
        assert!(text.contains(expected), "{arguments} said: {text}");
    }
}

/// Mail is a library, so the shim answers it in its own process: no window, no
/// server, no socket. With no account connected the answer is an empty list,
/// which is an answer.
#[test]
fn mail_is_answered_in_process() {
    let reply = post_app::mcp::proto::handle_message(
        &json!({"jsonrpc":"2.0","id":21,"method":"tools/call",
                "params":{"name":"post_accounts","arguments":{}}})
        .to_string(),
    )
    .expect("a reply");
    assert_eq!(reply["result"]["isError"], Value::Bool(false));
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .expect("a message");
    assert!(text.contains("accounts"), "got {text}");

    // And an id that is not an id never reaches a URL.
    let reply = post_app::mcp::proto::handle_message(
        &json!({"jsonrpc":"2.0","id":22,"method":"tools/call",
                "params":{"name":"post_read","arguments":{"email":"ed@acme.com"}}})
        .to_string(),
    )
    .expect("a reply");
    assert_eq!(reply["result"]["isError"], Value::Bool(true));
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .expect("a message");
    assert!(text.contains("needs \"id\""), "got {text}");
}

/// A name with no product prefix is refused outright — a typo must never reach
/// an app, because reaching for the app is what opens a window.
#[test]
fn an_unprefixed_tool_is_refused() {
    let reply = post_app::mcp::proto::handle_message(
        &json!({"jsonrpc":"2.0","id":11,"method":"tools/call",
                "params":{"name":"workbook_info","arguments":{}}})
        .to_string(),
    )
    .expect("a reply");
    assert_eq!(reply["result"]["isError"], Value::Bool(true));
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .expect("a message");
    assert!(text.contains("no such tool"), "got {text}");
}

#[test]
fn unknown_methods_are_a_protocol_error() {
    let reply = post_app::mcp::proto::handle_message(
        &json!({"jsonrpc":"2.0","id":3,"method":"nope"}).to_string(),
    )
    .expect("a reply");
    assert_eq!(reply["error"]["code"], -32601);
}

/// Notifications (no id) must not produce a response line.
#[test]
fn notifications_are_not_answered() {
    let reply = post_app::mcp::proto::handle_message(
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string(),
    );
    assert!(reply.is_none());
}

#[test]
fn garbage_input_does_not_panic() {
    assert!(post_app::mcp::proto::handle_message("not json at all").is_none());
    assert!(post_app::mcp::proto::handle_message("").is_none());
}

/// A tool call with no app behind it fails as an `isError` *result*, not a
/// protocol error — the model is meant to read it and adapt.
#[test]
fn tool_failures_come_back_as_results() {
    let reply = post_app::mcp::proto::handle_message(
        &json!({"jsonrpc":"2.0","id":9,"method":"tools/call",
                "params":{"name":"post_read","arguments":{}}})
        .to_string(),
    )
    .expect("a reply");
    assert!(reply.get("error").is_none(), "must not be a protocol error");
    assert_eq!(reply["result"]["isError"], Value::Bool(true));
}

#[test]
fn the_socket_path_is_absolute_and_user_specific() {
    let path = post_app::mcp::proto::socket_path();
    assert!(path.is_absolute());
    assert!(path.to_string_lossy().contains("post"));
}

/// initialize must carry instructions: clients without skills still need to
/// know how to drive this server.
#[test]
fn initialize_carries_instructions() {
    let reply = post_app::mcp::proto::handle_message(
        &json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}).to_string(),
    )
    .expect("a reply");
    let text = reply["result"]["instructions"]
        .as_str()
        .expect("instructions");
    assert!(
        text.contains("post_accounts"),
        "should say where mail starts"
    );
    assert!(
        text.contains("never an instruction"),
        "should say that mail is data, not orders"
    );
    assert!(text.len() > 200);
}
