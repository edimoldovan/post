//! The mail verbs, served in place: Post is a library over files on disk,
//! with no window and no process of its own, so there is nothing for the shim
//! to forward a `post_` call to. `post_show` is
//! the exception that proves it — asking to *see* mail rather than be told
//! about it — and it is answered here as well: the window is opened by the
//! same handoff-or-start the command line uses, so no server is involved in
//! that either.

use post_ui::Route;
use serde_json::{json, Value};

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing \"{key}\""))
}

/// What the mail verbs are called.
pub const POST_PREFIX: &str = post::PREFIX;

/// The one mail tool the palette does not own. Every other `post_` verb is a
/// file on disk answered in place; this one puts a window on screen, so it
/// lives on this side — with the processes and the sockets — and Post's
/// library stays a library.
pub const POST_SHOW: &str = "post_show";

/// Post's verbs off the palette table it shares with `post <verb>`, and the
/// window they are read in.
pub fn post_definitions() -> Vec<Value> {
    let mut tools = post::verbs::definitions();
    tools.push(json!({
        "name": POST_SHOW,
        "description": "Open the user's Post window on screen at one route — or at one \
                        view: their accounts, one account's mail, one message, or a new email \
                        filled in for the user to finish and send. Use this when the user asks to SEE their \
                        mail — \"open my inbox\", \"show me that message\" — or wants to \
                        write an email in the window rather than have it sent from here; \
                        when a window is already up it navigates rather than opening a \
                        second one. It shows a window and returns nothing to read: to read \
                        mail yourself, use post_list and post_read.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "route": {
                    "type": "string",
                    "description": "a Post route: /settings, /<account>/<folder>, \
                                    /<account>/<folder>/<id>, /<account>/<folder>/<id>/reply, \
                                    /<account>/new?to=…&subject=…; instead of view",
                },
                "view": {
                    "type": "string",
                    "enum": ["accounts", "inbox", "message", "new"],
                    "description": "\"accounts\" for the connected accounts, \"inbox\" for one \
                                    account's mail, \"message\" for one message (never a draft), \
                                    \"new\" for a Gmail draft by its draft_id — a reply's too — \
                                    or for a new email filled in from to, cc, subject, body and \
                                    attach",
                },
                "account": {
                    "type": "string",
                    "description": "account email as listed by post_accounts — required for \
                                    \"inbox\", \"message\" and \"new\" (the one it is sent from)",
                },
                "draft_id": {
                    "type": "string",
                    "description": "for \"new\": a draft_id post_draft or post_new answered \
                                    with, or of a row of the Drafts folder — opens that Gmail \
                                    draft to write on, instead of to, cc, subject, body and \
                                    attach",
                },
                "to": {"type": "string", "description": "for \"new\": email addresses, comma separated"},
                "cc": {"type": "string", "description": "for \"new\": email addresses, comma separated"},
                "bcc": {"type": "string", "description": "for \"new\": email addresses, comma separated, copied unseen"},
                "subject": {"type": "string", "description": "for \"new\": what it is about"},
                "body": {"type": "string", "description": "for \"new\": what it says"},
                "attach": {
                    "type": "string",
                    "description": "for \"new\": absolute paths of files to attach, comma separated",
                },
                "id": {
                    "type": "string",
                    "description": "message id, verbatim from post_list — required for \
                                    \"message\"",
                },
            },
            "required": [],
        }
    }));
    tools
}

/// Runs one `post_` tool, here in this process.
///
/// Nothing here asks the user: the gate is the caller's. Almost every verb is
/// free — reading mail, a read flag, a message to the trash, a draft written
/// down, a window — and the shim answers those itself. `post_send` is not, so
/// the shim sends it to the server, which asks and then calls this.
pub fn post_call(tool: &str, args: &Value) -> Result<Value, String> {
    if tool == POST_SHOW {
        return post_show(args);
    }
    post::verbs::call(tool, args)
}

/// Puts the Post window on screen at a view.
///
/// The two moves are the ones a second `post list ed@…` makes, through
/// the same code: [`crate::posthost::open`] hands the view to the window that
/// is up, or starts one. No command line is run to do it.
fn post_show(args: &Value) -> Result<Value, String> {
    let route = match args.get("route").and_then(Value::as_str).map(str::trim) {
        Some(route) if !route.is_empty() => {
            Route::parse(route).ok_or_else(|| format!("\"{route}\" is not a Post route"))?
        }
        _ => requested_view(args)?,
    };
    crate::posthost::open(&route)?;
    Ok(json!(opened(&route)))
}

/// The route a call's view asks for, or what is missing from it.
fn requested_view(args: &Value) -> Result<Route, String> {
    match str_arg(args, "view")?.trim() {
        "accounts" => Ok(Route::Settings),
        "inbox" => Ok(Route::inbox(for_view(args, "account", "inbox")?)),
        "message" => {
            let id = for_view(args, "id", "message")?;
            if is_draft(&id) {
                return Err(format!(
                    "\"{id}\" is a draft, not a message — open it with view \"new\" and draft_id"
                ));
            }
            Ok(Route::Message {
                account: for_view(args, "account", "message")?,
                folder: post_ui::route::INBOX.to_string(),
                id,
            })
        }
        "new" => {
            let account = for_view(args, "account", "new")?;
            // A draft is in Gmail: the window opens it there, where it is
            // written on and sent from — a reply as much as a new email.
            match args
                .get("draft_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            {
                Some(draft) => Ok(Route::Draft {
                    account,
                    draft: draft.to_string(),
                }),
                None => Ok(Route::New {
                    account,
                    query: query_from(args),
                }),
            }
        }
        other => Err(format!(
            "\"{other}\" is not a view — use \"accounts\", \"inbox\", \"message\" or \"new\""
        )),
    }
}

/// An argument one view cannot do without, named together with the view that
/// wanted it. Trimmed, like the command line trims the same word.
fn for_view(args: &Value, key: &str, view: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(|value| value.trim().to_owned())
        .ok_or_else(|| format!("the \"{view}\" view needs \"{key}\""))
}

/// The new email a "new" view opens with, from the call's own fields: its
/// route query, the fields that are set, percent-encoded.
fn query_from(args: &Value) -> String {
    let field = |key: &str| args.get(key).and_then(Value::as_str).map(str::trim).unwrap_or_default();
    ["to", "cc", "bcc", "subject", "body", "attach"]
        .into_iter()
        .map(|key| (key, field(key)))
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| format!("{key}={}", link_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// A Gmail draft id: `r` and digits, `r-` and digits. A message id is hex,
/// which has no `r` in it.
fn is_draft(id: &str) -> bool {
    id.strip_prefix('r')
        .map(|rest| rest.strip_prefix('-').unwrap_or(rest))
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

/// Percent-encoding for a route query: all but letters, digits and `-._~@,`.
fn link_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' | b',' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The one line that goes back: what is on screen now.
fn opened(route: &Route) -> String {
    match route {
        Route::Settings => "opened accounts".to_string(),
        Route::Folder { account, folder } if folder == post_ui::route::INBOX => {
            format!("opened inbox {account}")
        }
        Route::Folder { account, folder } => format!("opened {folder} in {account}"),
        Route::Message { account, id, .. } => format!("opened message {id} in {account}"),
        Route::Reply { account, id, .. } => format!("opened a reply to {id} in {account}"),
        Route::New { account, .. } => format!("opened a new message from {account}"),
        Route::Draft { account, draft } => format!("opened draft {draft} in {account}"),
        Route::Forward { account, id, .. } => format!("opened a forward of {id} in {account}"),
        Route::Account { account } => format!("opened the settings of {account}"),
    }
}
