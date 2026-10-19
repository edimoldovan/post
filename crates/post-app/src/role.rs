//! Which role a command line asks for.
//!
//! One executable, several jobs, and the arguments alone decide which — so the
//! binary an assistant launches, the server it starts, the dialog that server
//! puts on screen, and the window are the same file on disk.

/// The word that makes a launch *be* the Post window rather than ask for one.
///
/// Nobody types it: a launcher writes it, and the process that reads it back is
/// the detached one that holds the event loop. It is what keeps the terminal's
/// process out of the window business entirely.
pub const WINDOW_ARG: &str = "--window";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// `post mcp` — speak MCP on stdin/stdout.
    Shim,
    /// `post <verb> …` — one mail verb, from a terminal. The third door
    /// beside the chat and the window, off the same palette.
    Post { words: Vec<String> },
    /// `post`, `post list <email>`, `post read <email> <id>` — the mail verbs
    /// that are views rather than answers. Nobody reads email in a terminal,
    /// so these open the window at that view: hand it to the window that is
    /// up, or start one, and give the prompt back.
    PostOpen { route: post_ui::Route },
    /// `post --window <view>` — be that window. Only a launcher writes this
    /// line, and the process that reads it is detached from whoever did.
    PostWindow { route: post_ui::Route },
    /// `post --serve` — be the server. Headless.
    Serve,
    /// `post --confirm` — be one confirmation dialog: the question on stdin,
    /// the answer on stdout.
    Confirm,
}

/// Reads the arguments after the program name.
pub fn of<S: AsRef<str>>(args: &[S]) -> Role {
    let words: Vec<String> = args.iter().map(|a| a.as_ref().to_string()).collect();
    match words.first().map(String::as_str) {
        Some("mcp") => return Role::Shim,
        Some("--serve") => return Role::Serve,
        Some("--confirm") => return Role::Confirm,
        _ => {}
    }
    // The launcher's own line first: `post --window <route>` is the process
    // that holds the event loop.
    if let [first, route] = words.as_slice()
        && first == WINDOW_ARG
        && let Some(route) = post_ui::Route::parse(route)
    {
        return Role::PostWindow { route };
    }
    // Some verbs are views — the palette itself says which — and open the
    // window; the rest answer in the terminal.
    match post_ui::Route::of_words(&words) {
        Some(route) => Role::PostOpen { route },
        None => Role::Post { words },
    }
}
