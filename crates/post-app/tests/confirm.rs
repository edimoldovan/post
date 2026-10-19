//! The confirmation broker: what is asked about, and what each answer means.
//!
//! The dialog itself cannot be exercised without a screen, so what is checked
//! here is everything behind it — the queue, the four ways a question ends, the
//! allowlist on disk, and the one line each way between the server and the
//! process that asks. `tests/headless.rs` drives that pipe end to end.

use std::time::Duration;

use post_app::confirm::{acting, summary, Answer, Broker, Decision, Request, DENIED};
use serde_json::json;

/// A scratch allowlist path nothing else will touch. Everything the tests write
/// lives under one directory, so clearing up after a run is one `rm`.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir()
        .join("post-tests")
        .join(format!("confirm-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch dir");
    dir.join("allowlist.toml")
}

/// Waits for the broker to have a question, so a test never races the thread
/// that is asking it.
fn question(broker: &Broker) -> post_app::confirm::Waiting {
    for _ in 0..200 {
        if let Some(w) = broker.front() {
            return w;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no question was ever asked");
}

#[test]
fn reading_never_asks() {
    let broker = Broker::new(scratch("reads"));
    for tool in [
        "post_accounts",
        "post_list",
        "post_labels",
        "post_read",
        "post_thread",
        "post_show",
    ] {
        assert!(
            broker.decide(tool, &json!({})).is_ok(),
            "{tool} should run without asking"
        );
    }
    assert_eq!(broker.waiting(), 0);
}

#[test]
fn an_allowed_tool_runs_without_asking() {
    let file = scratch("allowed");
    std::fs::write(&file, "allow = [\"post_draft_delete\"]\n").expect("write");
    let broker = Broker::new(file);
    assert!(broker.allowed("post_draft_delete"));
    assert!(broker
        .decide("post_draft_delete", &json!({"email": "ed@acme.com", "draft_id": "r-1"}))
        .is_ok());
    assert_eq!(broker.waiting(), 0, "nothing should be queued");
}

#[test]
fn approving_lets_the_call_through() {
    let broker = std::sync::Arc::new(Broker::new(scratch("approve")));
    let asking = broker.clone();
    let call = std::thread::spawn(move || {
        asking.decide("post_draft_delete", &json!({"email": "ed@acme.com", "draft_id": "r-1"}))
    });

    let w = question(&broker);
    assert_eq!(w.tool, "post_draft_delete");
    assert!(
        w.summary.to_lowercase().contains("draft"),
        "the sentence should say what happens, got {:?}",
        w.summary
    );
    broker.resolve(w.id, Decision::Approve, false);

    assert!(call.join().expect("the call thread").is_ok());
    assert_eq!(broker.waiting(), 0);
    // One approval is one approval: the next call asks again.
    assert!(!broker.allowed("post_draft_delete"));
}

#[test]
fn denying_comes_back_as_denied() {
    let broker = std::sync::Arc::new(Broker::new(scratch("deny")));
    let asking = broker.clone();
    let call = std::thread::spawn(move || asking.decide("post_draft_delete", &json!({"email": "ed@acme.com", "draft_id": "r-1"})));

    let w = question(&broker);
    broker.resolve(w.id, Decision::Deny, false);

    let error = call.join().expect("the call thread").expect_err("a refusal");
    assert_eq!(error, DENIED);
    assert_eq!(broker.waiting(), 0);
}

/// Nobody at the keyboard is a no. The waiting call gives up on its own even
/// if no window is running to time it out.
#[test]
fn no_answer_is_a_refusal() {
    let broker = std::sync::Arc::new(Broker::with_timeout(
        scratch("timeout"),
        Duration::from_millis(50),
    ));
    let asking = broker.clone();
    let call = std::thread::spawn(move || asking.decide("post_draft_delete", &json!({})));

    let error = call.join().expect("the call thread").expect_err("a refusal");
    assert!(error.starts_with(DENIED), "got {error:?}");
    assert!(error.contains("no answer"), "should say why: {error:?}");
    assert_eq!(broker.waiting(), 0, "the question should be gone");
}

/// The server-side fuse, burning without a dialog anywhere. The thread that
/// asks the questions calls `expire` as it waits, which is what keeps a question
/// queued behind a long-lived dialog from outliving its countdown.
#[test]
fn the_countdown_refuses_it() {
    let broker = std::sync::Arc::new(Broker::with_timeout(
        scratch("expire"),
        Duration::from_millis(60),
    ));
    let asking = broker.clone();
    let call = std::thread::spawn(move || asking.decide("post_draft_delete", &json!({"email": "ed@acme.com", "draft_id": "r-1"})));

    let w = question(&broker);
    assert!(w.left <= 1);
    std::thread::sleep(Duration::from_millis(90));
    broker.expire();

    let error = call.join().expect("the call thread").expect_err("a refusal");
    assert_eq!(error, DENIED);
    assert_eq!(broker.waiting(), 0);
}

#[test]
fn always_allow_is_written_down_and_remembered() {
    let file = scratch("always");
    let _ = std::fs::remove_file(&file);
    let broker = std::sync::Arc::new(Broker::new(file.clone()));

    let first = {
        let asking = broker.clone();
        std::thread::spawn(move || asking.decide("post_draft_delete", &json!({"email": "ed@acme.com", "draft_id": "r-1"})))
    };
    let w = question(&broker);
    broker.resolve(w.id, Decision::Approve, true);
    assert!(first.join().expect("the call thread").is_ok());

    assert!(broker.allowed("post_draft_delete"));
    // The next one does not ask at all.
    assert!(broker.decide("post_draft_delete", &json!({})).is_ok());
    assert_eq!(broker.waiting(), 0);

    let written = std::fs::read_to_string(&file).expect("an allowlist file");
    assert!(written.contains("post_draft_delete"), "got {written:?}");
    // And a server started tomorrow reads the same answer.
    assert!(Broker::new(file).allowed("post_draft_delete"));
}

/// Saying "always" while more of the same tool is queued answers those too.
#[test]
fn always_allow_releases_what_is_already_queued() {
    let broker = std::sync::Arc::new(Broker::new(scratch("queued")));
    let calls: Vec<_> = (0..3)
        .map(|_| {
            let asking = broker.clone();
            std::thread::spawn(move || asking.decide("post_draft_delete", &json!({"email": "ed@acme.com", "draft_id": "r-1"})))
        })
        .collect();
    // Wait for all three to be queued before answering any.
    for _ in 0..200 {
        if broker.waiting() == 3 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(broker.waiting(), 3);

    let w = question(&broker);
    broker.resolve(w.id, Decision::Approve, true);
    for call in calls {
        assert!(call.join().expect("the call thread").is_ok());
    }
    assert_eq!(broker.waiting(), 0);
}

/// The acting set, tool by tool, against the whole advertised surface: if a
/// verb changes something it is asked about, and if it only looks it is not.
#[test]
fn every_advertised_tool_is_classified() {
    // Mail's free surface: it reads, it flips a read flag, it moves a message
    // to a trash it can be pulled back out of, it writes a reply down without
    // sending it, or it puts the window on screen. Named one by one so the next
    // verb has to be thought about rather than swept in. post_send is not
    // here, and neither is post_draft_delete — a draft deleted is gone for
    // good — nor post_remove, which takes an account off this computer.
    const MAIL: [&str; 15] = [
        "post_accounts",
        "post_rename",
        "post_forward",
        "post_add_account",
        "post_list",
        "post_labels",
        "post_draft_read",
        "post_read",
        "post_thread",
        "post_mark",
        "post_delete",
        "post_attachment",
        "post_draft",
        "post_new",
        "post_show",
    ];
    let mut acted = 0;
    let mut read = 0;
    for tool in post_app::mcp::proto::definitions() {
        let name = tool["name"].as_str().expect("every tool has a name");
        if MAIL.contains(&name) {
            assert!(!acting(name), "{name} only looks; it must not ask");
            read += 1;
        } else {
            assert!(acting(name), "{name} acts; it must be asked about");
            acted += 1;
        }
    }
    // The whole surface, not a subset of it: 17 post verbs plus post_show.
    assert_eq!(read + acted, 18, "the tool count changed; check this list");
    assert_eq!(read, 15);
    // Opening the window is free however it is asked for: it shows the user
    // their own mail and changes nothing.
    assert!(!acting("post_show"));
    // A verb added later under a known prefix is asked about until someone says
    // otherwise.
    assert!(acting("post_teleport"));
    // Sending mail acts, and can never be allowed for good: every reply is its
    // own question. So is taking an account off this computer.
    assert!(acting("post_send"));
    assert!(post_app::confirm::always_asks("post_send"));
    assert!(post_app::confirm::always_asks("post_remove"));
    assert!(!post_app::confirm::always_asks("post_draft"));
    // Nothing outside the product's own names is in the set at all.
    assert!(!acting("workbook_info"));
    assert!(!acting("rm"));
}

/// What the dialog process is handed: everything it draws, and no way to reach
/// back into the server.
#[test]
fn the_question_travels_as_one_line() {
    let broker = std::sync::Arc::new(Broker::new(scratch("wire")));
    let asking = broker.clone();
    let call = std::thread::spawn(move || {
        asking.decide("post_draft_delete", &json!({"email": "ed@acme.com", "draft_id": "r-1"}))
    });
    let w = question(&broker);

    let request = Request::about(&w);
    assert_eq!(request.tool, "post_draft_delete");
    assert_eq!(request.sentence, w.summary);
    assert_eq!(request.always_label, "Always allow post_draft_delete");
    assert!(request.seconds > 0);

    let line = request.encode();
    assert!(!line.contains('\n'), "one question is one line: {line}");
    assert_eq!(Request::parse(&line), Some(request));
    // Nothing that could be used to answer a different question.
    let sent: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert!(sent.get("id").is_none(), "the dialog knows no ids: {line}");

    broker.resolve(w.id, Decision::Deny, false);
    let _ = call.join();
}

/// The answer, and every way of not giving one.
#[test]
fn only_a_clean_approval_is_an_approval() {
    let approve = Answer {
        decision: Decision::Approve,
        always_allow: true,
    };
    let line = approve.encode();
    assert!(!line.contains('\n'), "one answer is one line: {line}");
    assert_eq!(Answer::parse(&line), Some(approve));
    assert_eq!(
        Answer::parse(&Answer::deny().encode()),
        Some(Answer {
            decision: Decision::Deny,
            always_allow: false
        })
    );
    // A trailing newline is how it arrives off a pipe.
    assert_eq!(
        Answer::parse("{\"decision\":\"approve\"}\n"),
        Some(Answer {
            decision: Decision::Approve,
            always_allow: false
        }),
        "no checkbox means no checkbox, not a broken answer"
    );
    for nothing in [
        "",
        "\n",
        "yes",
        "{}",
        "{\"decision\":\"maybe\"}",
        "{\"decision\":true}",
        "{\"always_allow\":true}",
    ] {
        assert_eq!(
            Answer::parse(nothing),
            None,
            "{nothing:?} is not an answer, so it is a refusal"
        );
    }
}

#[test]
fn the_sentence_says_what_will_happen() {
    let delete = summary("post_draft_delete", &json!({"email": "ed@acme.com", "draft_id": "r-1"}));
    assert!(delete.contains("r-1"), "got {delete:?}");
    assert!(delete.contains("could not be read"), "got {delete:?}");
}
