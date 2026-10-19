//! One executable, several roles, decided by the command line alone.

use post_app::role::{self, Role};

fn role(args: &[&str]) -> Role {
    role::of(args)
}

#[test]
fn the_assistant_launches_the_shim() {
    assert_eq!(role(&["mcp"]), Role::Shim);
}

#[test]
fn the_server_is_asked_for_by_flag() {
    assert_eq!(role(&["--serve"]), Role::Serve);
}

/// How the server puts a question on screen: this same binary, one dialog.
#[test]
fn the_dialog_is_asked_for_by_flag() {
    assert_eq!(role(&["--confirm"]), Role::Confirm);
}

/// The terminal door onto the palette: the verb and its arguments, flags
/// included — and no window opens.
#[test]
fn the_words_are_the_verbs() {
    for (line, words) in [
        ("post accounts", vec!["accounts"]),
        ("post add-account", vec!["add-account"]),
        ("post mark ed@acme.com 18f3 --read", vec![
            "mark",
            "ed@acme.com",
            "18f3",
            "--read",
        ]),
        ("post delete ed@acme.com 18f3", vec!["delete", "ed@acme.com", "18f3"]),
        ("post attachment ed@acme.com 18f3", vec![
            "attachment",
            "ed@acme.com",
            "18f3",
        ]),
        // A page of a listing has no view to be, so it stays a printed page.
        ("post list ed@acme.com --page tok3n", vec![
            "list",
            "ed@acme.com",
            "--page",
            "tok3n",
        ]),
        // Misuse still reaches the palette, which is where the usage line is.
        ("post list", vec!["list"]),
        ("post read ed@acme.com", vec!["read", "ed@acme.com"]),
        ("post --help", vec!["--help"]),
        ("post sendmail", vec!["sendmail"]),
    ] {
        let args: Vec<&str> = line.split(' ').skip(1).collect();
        assert_eq!(
            role(&args),
            Role::Post {
                words: words.into_iter().map(str::to_string).collect()
            },
            "{line}"
        );
    }
}

/// The mail verbs that are views rather than answers open the window at that
/// route. Nobody reads email in a terminal.
#[test]
fn the_mail_verbs_that_are_views_open_the_window() {
    use post_ui::{Route, route::INBOX};

    assert_eq!(role(&[]), Role::PostOpen { route: Route::home() });
    assert_eq!(
        role(&["list", "ed@acme.com"]),
        Role::PostOpen {
            route: Route::inbox("ed@acme.com")
        }
    );
    // Not a file to open, and not the server either.
    assert_eq!(
        role(&["read", "ed@acme.com", "18f3a2c9b1"]),
        Role::PostOpen {
            route: Route::Message {
                account: "ed@acme.com".to_string(),
                folder: INBOX.to_string(),
                id: "18f3a2c9b1".to_string(),
            }
        }
    );
}

/// And the line a launcher writes: one route, being the window rather than
/// asking for one. Nobody types this.
#[test]
fn the_window_itself_is_a_role_of_its_own() {
    use post_ui::{Route, route::INBOX};

    assert_eq!(
        role(&["--window", "/settings"]),
        Role::PostWindow {
            route: Route::Settings
        }
    );
    assert_eq!(
        role(&["--window", "/ed@acme.com/INBOX"]),
        Role::PostWindow {
            route: Route::inbox("ed@acme.com")
        }
    );
    assert_eq!(
        role(&["--window", "/ed@acme.com/INBOX/18f3a2c9b1"]),
        Role::PostWindow {
            route: Route::Message {
                account: "ed@acme.com".to_string(),
                folder: INBOX.to_string(),
                id: "18f3a2c9b1".to_string(),
            }
        }
    );
    // A flag that names no route is a verb the palette answers, not a window.
    assert_eq!(
        role(&["--window", "sendmail"]),
        Role::Post {
            words: vec!["--window".to_string(), "sendmail".to_string()]
        }
    );
}
