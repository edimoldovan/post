//! One Post window, and where a second `post …` goes.
//!
//! All of this runs with no display: the handover is a socket and a channel,
//! and the route that travels on it is decided before anything is drawn. What
//! is checked here is the part a person notices — typing the command twice
//! gives one window that navigates, and the second command exits instead of
//! waiting.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use post_app::ipc::{self, Msg};
use post_app::posthost::{self, Outcome};
use post_app::role::{self, Role};
use post_ui::{Route, route::INBOX};

/// A scratch runtime directory, standing in for `$XDG_RUNTIME_DIR`.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("post-tests")
        .join(format!("post-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch dir");
    dir
}

/// The route the window was handed, or nothing within a second.
fn handed(rx: &std::sync::mpsc::Receiver<Route>) -> Option<Route> {
    rx.recv_timeout(Duration::from_secs(1)).ok()
}

/// A message in the inbox.
fn message(account: &str, id: &str) -> Route {
    Route::Message {
        account: account.to_string(),
        folder: INBOX.to_string(),
        id: id.to_string(),
    }
}

/// The first launch is the window; the second gives it the route and goes.
#[test]
fn the_second_command_navigates_the_window_that_is_already_up() {
    let dir = scratch("handoff");
    let socket = dir.join("post-window.sock");

    let Outcome::Run(rx) = posthost::start_at(&socket, &Route::Settings) else {
        panic!("the first launch is the window");
    };
    assert!(socket.exists(), "the window took the socket");

    // A second `post list ed@acme.com`.
    match posthost::start_at(&socket, &Route::inbox("ed@acme.com")) {
        Outcome::HandedOff => {}
        Outcome::Run(_) => panic!("a second window opened"),
    }
    assert_eq!(handed(&rx), Some(Route::inbox("ed@acme.com")));

    // And a third, at a message this time.
    let detail = message("ed@acme.com", "18f3a2c9b1");
    assert!(matches!(
        posthost::start_at(&socket, &detail),
        Outcome::HandedOff
    ));
    assert_eq!(handed(&rx), Some(detail));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A socket file left behind by a crash answers nothing, so it is replaced —
/// never respected, and never left to make a launch hang.
#[test]
fn a_socket_a_crash_left_behind_is_taken_over() {
    let dir = scratch("stale");
    let socket = dir.join("post-window.sock");
    std::fs::write(&socket, b"not a socket").expect("a stale file");

    let Outcome::Run(rx) = posthost::start_at(&socket, &Route::Settings) else {
        panic!("the stale file should not hold the door");
    };
    assert!(matches!(
        posthost::start_at(&socket, &Route::inbox("ed@acme.com")),
        Outcome::HandedOff
    ));
    assert_eq!(handed(&rx), Some(Route::inbox("ed@acme.com")));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A route arrives on the socket as its path, and comes back out the same
/// route — that is the whole wire.
#[test]
fn a_route_travels_as_an_op_and_arrives_as_itself() {
    let dir = scratch("framing");
    let socket = dir.join("post-window.sock");
    let Outcome::Run(rx) = posthost::start_at(&socket, &Route::Settings) else {
        panic!("the first launch is the window");
    };

    let mut stream = UnixStream::connect(&socket).expect("the window answers");
    ipc::send(
        &mut stream,
        &Msg::View {
            id: Some(9),
            route: "/ale@acme.com/INBOX/18f2".to_string(),
        },
    )
    .expect("sent");
    let mut reader = BufReader::new(stream.try_clone().expect("a reader"));
    assert!(matches!(
        ipc::recv(&mut reader),
        Ok(Some(Msg::Reply {
            id: Some(9),
            result: Ok(_)
        }))
    ));
    assert_eq!(handed(&rx), Some(message("ale@acme.com", "18f2")));

    // Something the window does not answer is refused, and the connection
    // stays up for whatever comes next.
    ipc::send(
        &mut stream,
        &Msg::Reply {
            id: Some(10),
            result: Ok(serde_json::json!("here")),
        },
    )
    .expect("sent");
    assert!(matches!(
        ipc::recv(&mut reader),
        Ok(Some(Msg::Reply {
            id: Some(10),
            result: Err(_)
        }))
    ));

    // So is a path that names no screen, and the window stays where it is.
    ipc::send(
        &mut stream,
        &Msg::View {
            id: Some(11),
            route: "/ale@acme.com/INBOX/18f2/forward/x".to_string(),
        },
    )
    .expect("sent");
    assert!(matches!(
        ipc::recv(&mut reader),
        Ok(Some(Msg::Reply {
            id: Some(11),
            result: Err(_)
        }))
    ));
    assert_eq!(handed(&rx), None);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A line that is not an op at all is ignored rather than answered, and the
/// window does not fall over reading it.
#[test]
fn a_line_that_is_not_an_op_moves_nothing() {
    let dir = scratch("garbage");
    let socket = dir.join("post-window.sock");
    let Outcome::Run(rx) = posthost::start_at(&socket, &Route::Settings) else {
        panic!("the first launch is the window");
    };

    let mut stream = UnixStream::connect(&socket).expect("the window answers");
    writeln!(stream, "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}}")
        .expect("sent");
    writeln!(stream, "not json at all").expect("sent");
    // The real errand, behind the noise.
    ipc::send(
        &mut stream,
        &Msg::View {
            id: Some(1),
            route: "/ed@acme.com/INBOX".to_string(),
        },
    )
    .expect("sent");

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) > 0 {
            break;
        }
    }
    assert!(ipc::is_op(&line), "the answer is an op: {line}");
    assert_eq!(handed(&rx), Some(Route::inbox("ed@acme.com")));

    let _ = std::fs::remove_dir_all(&dir);
}

/// The command line, the role, the route and the path on the socket are one
/// road with no forks in it.
#[test]
fn the_command_line_and_the_wire_agree_on_the_route() {
    for (args, route) in [
        (vec![], Route::home()),
        (vec!["list", "ed@acme.com"], Route::inbox("ed@acme.com")),
        (
            vec!["read", "ed@acme.com", "18f3"],
            message("ed@acme.com", "18f3"),
        ),
        (
            vec!["reply", "ed@acme.com", "18f3"],
            Route::Reply {
                account: "ed@acme.com".to_string(),
                folder: INBOX.to_string(),
                id: "18f3".to_string(),
            },
        ),
        (
            vec!["open", "/ed@acme.com/SENT"],
            Route::Folder {
                account: "ed@acme.com".to_string(),
                folder: "SENT".to_string(),
            },
        ),
    ] {
        // What a person types asks for the window; it never becomes one.
        assert_eq!(
            role::of(&args),
            Role::PostOpen {
                route: route.clone()
            },
            "{args:?}"
        );
        // And the socket carries the same route as its path.
        assert_eq!(Route::parse(&route.to_string()), Some(route));
    }
}

/// A process that will never be the window — the assistant answering
/// `post_show` — opens one the same two ways: the window that is up takes the
/// route, and when none is up a window process is started at it.
#[test]
fn a_windowless_process_hands_off_or_starts_the_window() {
    let dir = scratch("open");
    let socket = dir.join("post-window.sock");

    // Nothing is listening, so a window is started — at the route that was
    // asked for, and by a role rather than a command line.
    let started: RefCell<Vec<String>> = RefCell::new(Vec::new());
    posthost::open_at(&socket, &Route::inbox("ed@acme.com"), |args| {
        started.borrow_mut().extend_from_slice(args);
        Ok(())
    })
    .expect("a window was started");
    assert_eq!(
        started.borrow().as_slice(),
        ["--window", "/ed@acme.com/INBOX"],
        "the window is started at the route"
    );
    assert_eq!(
        role::of(started.borrow().as_slice()),
        Role::PostWindow {
            route: Route::inbox("ed@acme.com")
        },
        "the process that starts reads back the same route"
    );
    assert!(!socket.exists(), "opening a window binds nothing here");

    // Now one is up: it takes the route, and nothing is started beside it.
    let Outcome::Run(rx) = posthost::start_at(&socket, &Route::Settings) else {
        panic!("the first launch is the window");
    };
    posthost::open_at(&socket, &Route::inbox("ale@acme.com"), |_| {
        panic!("a second window was started while one was up")
    })
    .expect("handed off");
    assert_eq!(handed(&rx), Some(Route::inbox("ale@acme.com")));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A route whose path would read back as some other route is refused before
/// either move, so `post_show` means the same thing whether a window is up or
/// not — never a different screen.
#[test]
fn a_route_that_cannot_be_named_opens_nothing() {
    let dir = scratch("unnameable");
    let socket = dir.join("post-window.sock");

    for route in [
        Route::inbox(""),
        Route::inbox("   "),
        Route::Folder {
            account: "ed@acme.com".to_string(),
            folder: "new".to_string(),
        },
    ] {
        assert!(
            posthost::open_at(&socket, &route, |_| panic!("started a window")).is_err(),
            "{route:?} opened something"
        );
    }

    // The same answer with a window up: it is not asked either.
    let Outcome::Run(rx) = posthost::start_at(&socket, &Route::Settings) else {
        panic!("the first launch is the window");
    };
    assert!(
        posthost::open_at(&socket, &Route::inbox(""), |_| panic!("started a window")).is_err()
    );
    assert_eq!(handed(&rx), None, "nothing was handed to the window");

    let _ = std::fs::remove_dir_all(&dir);
}

/// However a route is asked for, it is one road: the route names the launch,
/// the launch names the role, and the role is that same route again.
#[test]
fn the_words_a_window_is_started_with_are_the_route_itself() {
    for route in [
        Route::Settings,
        Route::inbox("ed@acme.com"),
        message("ed@acme.com", "18f3a2c9b1"),
        Route::Reply {
            account: "ed@acme.com".to_string(),
            folder: INBOX.to_string(),
            id: "18f3a2c9b1".to_string(),
        },
        Route::New {
            account: "ed@acme.com".to_string(),
            query: "to=ale%40acme.com&subject=Hi%20there".to_string(),
        },
    ] {
        let args = posthost::launch_args(&route);
        assert_eq!(args, ["--window", route.to_string().as_str()]);
        assert_eq!(
            role::of(&args),
            Role::PostWindow {
                route: route.clone()
            },
            "{route}"
        );
    }
}

/// The command a person types never becomes the window: it hands the route to
/// the window that is up, or starts one and goes. Either way the prompt comes
/// back at once, and the window it started is a process of its own.
#[test]
fn the_command_line_asks_for_the_window_and_never_becomes_one() {
    let dir = scratch("detach");
    let socket = dir.join("post-window.sock");

    // Whatever a person can type, the role is the one that hands off.
    for args in [
        vec![],
        vec!["list", "ed@acme.com"],
        vec!["read", "ed@acme.com", "18f3"],
    ] {
        assert!(
            matches!(role::of(&args), Role::PostOpen { .. }),
            "{args:?} would have held the terminal"
        );
    }

    // Nothing is listening, so a window is started — and it is the started
    // process, not this one, that reads back the role which holds the loop.
    let started: RefCell<Vec<String>> = RefCell::new(Vec::new());
    posthost::open_at(&socket, &Route::inbox("ed@acme.com"), |args| {
        started.borrow_mut().extend_from_slice(args);
        Ok(())
    })
    .expect("a window was started");
    assert_eq!(
        role::of(started.borrow().as_slice()),
        Role::PostWindow {
            route: Route::inbox("ed@acme.com")
        },
        "the started process is the window"
    );
    assert!(!socket.exists(), "the command bound nothing of its own");

    // And with a window up, nothing is started at all.
    let Outcome::Run(rx) = posthost::start_at(&socket, &Route::Settings) else {
        panic!("the first launch is the window");
    };
    posthost::open_at(&socket, &Route::inbox("ale@acme.com"), |_| {
        panic!("a second window was started while one was up")
    })
    .expect("handed off");
    assert_eq!(handed(&rx), Some(Route::inbox("ale@acme.com")));

    let _ = std::fs::remove_dir_all(&dir);
}

/// Post's door is its own, beside the app socket rather than on it — a mail
/// window must never be mistaken for the assistant's server.
#[test]
fn the_window_keeps_its_own_socket() {
    let post = posthost::socket_path();
    let app = post_app::mcp::proto::socket_path();
    assert_ne!(post, app);
    assert_eq!(post.parent(), app.parent());
    assert!(
        post.file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with("-window.sock")),
        "got {}",
        post.display()
    );
}
