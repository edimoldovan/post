//! Post's routes, and the history of them.
//!
//! Every screen is a route — a path that names it — and the router holds the
//! routes walked through, the one on screen on top. Back retraces steps that
//! were taken: the window opens at one route and that route is the floor, so
//! `post list ed@…` lands in an inbox with nothing behind it, while
//! opening a message from that inbox puts the inbox behind the message.
//!
//! This module knows nothing about egui. It is also where a command line, a
//! second launch and the assistant name a screen: all of them name a route.
//!
//! | Route | Screen |
//! |---|---|
//! | `/` | the first account's inbox, or `/settings` with no account |
//! | `/settings` | the accounts, and Add account |
//! | `/:account` | the same as `/:account/INBOX` |
//! | `/:account/:folder` | one folder's messages |
//! | `/:account/:folder/:id` | the conversation, message `:id` open |
//! | `/:account/:folder/:id/reply` | a reply to `:id` |
//! | `/:account/new?to=&cc=&subject=&body=&attach=` | a new email |

use std::fmt;

/// The folder a message is opened from when nothing says which.
pub const INBOX: &str = "INBOX";

/// A search is a folder of its own: `search/<query>` where a label id would
/// be. No label id looks like that — Gmail's are `INBOX` or `Label_12`.
pub const SEARCH: &str = "search/";

/// The folder a search for `query` lists.
pub fn searching(query: &str) -> String {
    format!("{SEARCH}{query}")
}

/// The query a folder searches for, when it is a search.
pub fn query_of(folder: &str) -> Option<&str> {
    folder.strip_prefix(SEARCH)
}

/// A folder as its path writes it: a label id, or `search/` and the query.
fn place(folder: &str) -> String {
    match query_of(folder) {
        Some(query) => format!("search/{}", encode(query)),
        None => encode(folder),
    }
}

/// What the window is showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// `/settings`: the connected accounts, and Add account.
    Settings,
    /// `/:account/:folder`: one folder of one account's mail.
    Folder { account: String, folder: String },
    /// `/:account/:folder/:id`: one message of that folder, its conversation
    /// around it.
    Message {
        account: String,
        folder: String,
        id: String,
    },
    /// `/:account/:folder/:id/reply`: a reply being written to one message.
    Reply {
        account: String,
        folder: String,
        id: String,
    },
    /// `/:account/new?…`: a new email from one account, its fields filled in
    /// from the query — `to`, `cc`, `bcc`, `subject`, `body`, `attach` — the
    /// way a `mailto:` link fills them.
    New { account: String, query: String },
    /// `/:account/drafts/:draft`: a Gmail draft, open in the new-email screen.
    Draft { account: String, draft: String },
    /// `/:account/:folder/:id/forward`: `:id` forwarded, with its attachments.
    Forward {
        account: String,
        folder: String,
        id: String,
    },
    /// `/settings/:account`: one account's settings — its name, and Remove.
    Account { account: String },
}

impl Route {
    /// An account's inbox.
    pub fn inbox(account: impl Into<String>) -> Route {
        Route::Folder {
            account: account.into(),
            folder: INBOX.to_string(),
        }
    }

    /// Where `/` goes: the first account's inbox, or settings with none.
    pub fn home() -> Route {
        match first_account() {
            Some(account) => Route::inbox(account),
            None => Route::Settings,
        }
    }

    /// The account this route is about, if any.
    pub fn account(&self) -> Option<&str> {
        match self {
            Route::Settings => None,
            Route::Folder { account, .. }
            | Route::Message { account, .. }
            | Route::Reply { account, .. }
            | Route::New { account, .. }
            | Route::Draft { account, .. }
            | Route::Forward { account, .. }
            | Route::Account { account } => Some(account),
        }
    }

    /// The folder this route is in, if any.
    pub fn folder(&self) -> Option<&str> {
        match self {
            Route::Folder { folder, .. }
            | Route::Message { folder, .. }
            | Route::Reply { folder, .. }
            | Route::Forward { folder, .. } => Some(folder),
            Route::Settings | Route::New { .. } | Route::Draft { .. } | Route::Account { .. } => {
                None
            }
        }
    }

    /// The message this route is *showing*, if any.
    ///
    /// A reply is not one: composing shows the reply being written, and the
    /// message it answers is behind it.
    pub fn message(&self) -> Option<&str> {
        match self {
            Route::Message { id, .. } => Some(id),
            _ => None,
        }
    }

    /// The message a reply is being written to.
    pub fn replying_to(&self) -> Option<&str> {
        match self {
            Route::Reply { id, .. } => Some(id),
            _ => None,
        }
    }

    /// The screen one level up: a message's folder, a reply's message. Back
    /// from a message or a reply goes there, and a message's folder is the
    /// list beside it on a wide window.
    pub fn parent(&self) -> Option<Route> {
        match self {
            Route::Message { account, folder, .. } => Some(Route::Folder {
                account: account.clone(),
                folder: folder.clone(),
            }),
            Route::Reply { account, folder, id } | Route::Forward { account, folder, id } => {
                Some(Route::Message {
                    account: account.clone(),
                    folder: folder.clone(),
                    id: id.clone(),
                })
            }
            Route::Account { .. } => Some(Route::Settings),
            _ => None,
        }
    }

    /// True while a message is being written: the keys are the editor's.
    pub fn writing(&self) -> bool {
        matches!(
            self,
            Route::Reply { .. } | Route::New { .. } | Route::Draft { .. } | Route::Forward { .. }
        )
    }

    /// Reads a route. `/` is the first account's inbox, or settings; a path
    /// that names no screen, or does not start with `/`, is `None`.
    pub fn parse(text: &str) -> Option<Route> {
        let text = text.strip_prefix('/')?;
        let (path, query) = text.split_once('?').unwrap_or((text, ""));
        let segments: Vec<String> = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(decode)
            .collect();
        let s: Vec<&str> = segments.iter().map(String::as_str).collect();
        let named = |word: &str| !word.trim().is_empty();
        match s.as_slice() {
            [] => Some(Route::home()),
            ["settings"] => Some(Route::Settings),
            ["settings", account] if named(account) => Some(Route::Account {
                account: account.to_string(),
            }),
            // A search, and what was opened from it. Before the label forms,
            // which would read `search` as a label id.
            [account, "search", query] if named(account) && named(query) => {
                Some(Route::Folder {
                    account: account.to_string(),
                    folder: searching(query),
                })
            }
            [account, "search", query, id] if [account, query, id].iter().all(|w| named(w)) => {
                Some(Route::Message {
                    account: account.to_string(),
                    folder: searching(query),
                    id: id.to_string(),
                })
            }
            [account, "search", query, id, "reply"]
                if [account, query, id].iter().all(|w| named(w)) =>
            {
                Some(Route::Reply {
                    account: account.to_string(),
                    folder: searching(query),
                    id: id.to_string(),
                })
            }
            [account, "search", query, id, "forward"]
                if [account, query, id].iter().all(|w| named(w)) =>
            {
                Some(Route::Forward {
                    account: account.to_string(),
                    folder: searching(query),
                    id: id.to_string(),
                })
            }
            // A draft: before the label forms, which would read `drafts` as a
            // label id. The Drafts folder itself is `DRAFT`.
            [account, "drafts", draft] if named(account) && named(draft) => Some(Route::Draft {
                account: account.to_string(),
                draft: draft.to_string(),
            }),
            [account, "new"] if named(account) => Some(Route::New {
                account: account.to_string(),
                query: query.to_string(),
            }),
            [account] if named(account) => Some(Route::inbox(*account)),
            [account, folder] if named(account) && named(folder) => Some(Route::Folder {
                account: account.to_string(),
                folder: folder.to_string(),
            }),
            [account, folder, id] if [account, folder, id].iter().all(|w| named(w)) => {
                Some(Route::Message {
                    account: account.to_string(),
                    folder: folder.to_string(),
                    id: id.to_string(),
                })
            }
            [account, folder, id, "reply"] if [account, folder, id].iter().all(|w| named(w)) => {
                Some(Route::Reply {
                    account: account.to_string(),
                    folder: folder.to_string(),
                    id: id.to_string(),
                })
            }
            [account, folder, id, "forward"] if [account, folder, id].iter().all(|w| named(w)) => {
                Some(Route::Forward {
                    account: account.to_string(),
                    folder: folder.to_string(),
                    id: id.to_string(),
                })
            }
            _ => None,
        }
    }

    /// The route `post <words>` asks for — or `None` when those words
    /// are a terminal verb, or are not a verb at all.
    ///
    /// The views have a command: `post`, `list`, `read`, `reply`, a
    /// `mailto:` link and `open <route>`. Everything else (`add-account`,
    /// `mark`, `delete`, `attachment`, `accounts`, `draft`, `send`, the help, a
    /// typo) stays in the terminal, where it already says the right thing. A
    /// page token has no screen to be, so `list --page …` stays a terminal
    /// listing too.
    pub fn of_words<S: AsRef<str>>(words: &[S]) -> Option<Route> {
        let words: Vec<&str> = words.iter().map(AsRef::as_ref).collect();
        match words.as_slice() {
            [] => Some(Route::home()),
            ["open", route] => Route::parse(route),
            ["list", email] => Some(Route::inbox(value(email)?)),
            ["read", email, id] => Some(Route::Message {
                account: value(email)?,
                folder: INBOX.to_string(),
                id: value(id)?,
            }),
            ["reply", email, id] => Some(Route::Reply {
                account: value(email)?,
                folder: INBOX.to_string(),
                id: value(id)?,
            }),
            // What the desktop hands a mail app for a mailto: link. It goes
            // from the first account connected; with none, there is nothing
            // to write from yet.
            [link] if is_mailto(link) => Some(match first_account() {
                Some(account) => Route::New {
                    account,
                    query: query_of_mailto(link),
                },
                None => Route::Settings,
            }),
            _ => None,
        }
    }
}

impl fmt::Display for Route {
    /// The path that names this route: `Route::parse` reads it back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Route::Settings => write!(f, "/settings"),
            Route::Folder { account, folder } => write!(f, "/{}/{}", encode(account), place(folder)),
            Route::Message { account, folder, id } => {
                write!(f, "/{}/{}/{}", encode(account), place(folder), encode(id))
            }
            Route::Reply { account, folder, id } => {
                write!(f, "/{}/{}/{}/reply", encode(account), place(folder), encode(id))
            }
            Route::Forward { account, folder, id } => {
                write!(f, "/{}/{}/{}/forward", encode(account), place(folder), encode(id))
            }
            Route::Account { account } => write!(f, "/settings/{}", encode(account)),
            Route::New { account, query } if query.is_empty() => write!(f, "/{}/new", encode(account)),
            Route::New { account, query } => write!(f, "/{}/new?{query}", encode(account)),
            Route::Draft { account, draft } => {
                write!(f, "/{}/drafts/{}", encode(account), encode(draft))
            }
        }
    }
}

/// A command-line word standing in for an address or an id. A flag is not one,
/// and neither is nothing at all: both belong to the terminal door, which says
/// what was missing.
fn value(word: &str) -> Option<String> {
    let word = word.trim();
    if word.is_empty() || word.starts_with('-') {
        return None;
    }
    Some(word.to_string())
}

/// The window's history: every step walked through, oldest first, each a route
/// and what its screen held when it was left. Never empty: the route the window
/// opened at is the floor.
///
/// The screen of the step on top is the window's own while it is on screen —
/// the router's copy of it is an empty stand-in. Every move hands the window's
/// screen in and gives back the one it arrives at: empty for a new step, as it
/// was left for a step behind.
pub struct Router<S> {
    stack: Vec<Step<S>>,
    /// The last key handed out. Keys are never reused, so an answer for a step
    /// that is gone finds nothing to land on.
    keys: u64,
}

struct Step<S> {
    route: Route,
    key: u64,
    screen: S,
}

impl<S: Default> Router<S> {
    /// A window opening at `route`, with the levels above it behind it — a
    /// message's folder, a reply's message — so Back goes up one level, and
    /// nothing else: the rest was not walked to.
    pub fn of(route: Route) -> Router<S> {
        let mut router = Router {
            stack: Vec::new(),
            keys: 0,
        };
        router.seed(route);
        router
    }

    /// What is on screen.
    pub fn top(&self) -> &Route {
        self.stack.last().map_or(&Route::Settings, |step| &step.route)
    }

    /// The step on screen's key: what a load started now answers to.
    pub fn key(&self) -> u64 {
        self.stack.last().map_or(0, |step| step.key)
    }

    /// The route one step back, if there is one.
    pub fn below(&self) -> Option<&Route> {
        let n = self.stack.len();
        (n > 1).then(|| &self.stack[n - 2].route)
    }

    /// How deep the stack is. One is the route the window opened at, where
    /// Back does nothing.
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// A step the reader took: the route we were on stays behind this one,
    /// with `screen` kept as it was left. Idempotent, and stepping back onto a
    /// route already behind us returns to it — screen and all — rather than
    /// stacking it again, so walking in circles stays shallow. A route whose
    /// level above is not on top steps through that level first, so Back
    /// always goes up one.
    pub fn push(&mut self, route: Route, screen: &mut S) {
        if self.top() == &route {
            return;
        }
        if self.return_to(&route, screen) {
            return;
        }
        if let Some(parent) = route.parent()
            && self.top() != &parent
        {
            self.push(parent, screen);
        }
        if let Some(top) = self.stack.last_mut() {
            top.screen = std::mem::take(screen);
        }
        self.step_onto(route);
    }

    /// The screen becomes another without adding a step: what was on top is
    /// dropped with its screen, and Back does not come back to it.
    pub fn replace(&mut self, route: Route, screen: &mut S) {
        if self.top() == &route {
            return;
        }
        self.stack.pop();
        match self.stack.last_mut() {
            // The step below is on screen again for a moment, to be left the
            // way any step is.
            Some(top) => {
                *screen = std::mem::take(&mut top.screen);
                self.push(route, screen);
            }
            None => {
                *screen = S::default();
                self.seed(route);
            }
        }
    }

    /// Back one step, to its screen as it was left; the one on screen is
    /// dropped. False at the floor, where there is nowhere to go.
    pub fn back(&mut self, screen: &mut S) -> bool {
        if self.stack.len() <= 1 {
            return false;
        }
        self.stack.pop();
        if let Some(top) = self.stack.last_mut() {
            *screen = std::mem::take(&mut top.screen);
        }
        true
    }

    /// The key of the step one back, if there is one.
    pub fn below_key(&self) -> Option<u64> {
        let n = self.stack.len();
        (n > 1).then(|| self.stack[n - 2].key)
    }

    /// The step one back — its key, route and screen — if there is one: the
    /// list beside a message on a wide window.
    pub fn below_mut(&mut self) -> Option<(u64, &Route, &mut S)> {
        let n = self.stack.len();
        if n < 2 {
            return None;
        }
        let Step { route, key, screen } = &mut self.stack[n - 2];
        Some((*key, &*route, screen))
    }

    /// Every step's key, route and screen, oldest first — `top` standing in
    /// for the one on screen, whose screen is the window's.
    pub fn screens<'a>(&'a mut self, top: &'a mut S) -> impl Iterator<Item = (u64, &'a Route, &'a mut S)> {
        let n = self.stack.len();
        let mut top = Some(top);
        self.stack.iter_mut().enumerate().filter_map(move |(i, step)| {
            let Step { route, key, screen } = step;
            let screen = if i + 1 == n { top.take()? } else { screen };
            Some((*key, &*route, screen))
        })
    }

    /// Back down to `route` if it is a step behind us, dropping everything
    /// above it; its screen comes back as it was left.
    fn return_to(&mut self, route: &Route, screen: &mut S) -> bool {
        let Some(at) = self.stack.iter().position(|step| &step.route == route) else {
            return false;
        };
        self.stack.truncate(at + 1);
        *screen = std::mem::take(&mut self.stack[at].screen);
        true
    }

    /// `route` and the levels above it, as fresh steps on top.
    fn seed(&mut self, route: Route) {
        if let Some(parent) = route.parent() {
            self.seed(parent);
        }
        self.step_onto(route);
    }

    /// A new step on top, under a key nobody has had.
    fn step_onto(&mut self, route: Route) {
        self.keys += 1;
        self.stack.push(Step {
            route,
            key: self.keys,
            screen: S::default(),
        });
    }
}

impl<S: Default> Default for Router<S> {
    fn default() -> Router<S> {
        Router::of(Route::Settings)
    }
}

fn is_mailto(word: &str) -> bool {
    word.len() >= 7 && word[..7].eq_ignore_ascii_case("mailto:")
}

/// A `mailto:` link as a new email's query: its addresses become `to`, and its
/// own fields come along as they are.
pub fn query_of_mailto(link: &str) -> String {
    let rest = if is_mailto(link) { &link[7..] } else { link };
    let (addrs, fields) = rest.split_once('?').unwrap_or((rest, ""));
    match (addrs.is_empty(), fields.is_empty()) {
        (true, _) => fields.to_string(),
        (false, true) => format!("to={addrs}"),
        (false, false) => format!("to={addrs}&{fields}"),
    }
}

/// The account a new message goes from when nothing says otherwise.
fn first_account() -> Option<String> {
    post::store::accounts(&post::paths::accounts_file())
        .into_iter()
        .next()
}

/// Percent-encoding for one path segment: all but letters, digits and
/// `-._~@+`.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' | b'+' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A `mailto:` link, read the way RFC 6068 writes it: the addresses before
/// the `?`, then subject, body, cc and to.
#[derive(Debug, Default, PartialEq)]
pub struct Mailto {
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
    /// Files to attach, by absolute path. Not RFC 6068: only Post's own
    /// links carry `attach=`, when the assistant fills a new message in.
    pub attach: Vec<String>,
}

impl Mailto {
    pub fn parse(link: &str) -> Mailto {
        let rest = if is_mailto(link) { &link[7..] } else { link };
        let (addrs, query) = rest.split_once('?').unwrap_or((rest, ""));
        let mut mail = Mailto {
            to: decode(addrs),
            ..Default::default()
        };
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = decode(value);
            match key.to_ascii_lowercase().as_str() {
                "subject" => mail.subject = value,
                "body" => mail.body = value.replace("\r\n", "\n"),
                "cc" => mail.cc = joined(&mail.cc, &value),
                "bcc" => mail.bcc = joined(&mail.bcc, &value),
                "to" => mail.to = joined(&mail.to, &value),
                "attach" => mail.attach.extend(
                    value.split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned),
                ),
                _ => {}
            }
        }
        mail
    }
}

fn joined(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => format!("{a}, {b}"),
    }
}

/// Percent-decoding. A `+` stays a plus: in a mailto link it is one.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn message(id: &str) -> Route {
        Route::Message {
            account: "ed@acme.com".to_string(),
            folder: INBOX.to_string(),
            id: id.to_string(),
        }
    }

    /// Every route reads back as itself from the path it writes.
    #[test]
    fn every_route_round_trips_through_its_path() {
        for route in [
            Route::Settings,
            Route::inbox("ed@acme.com"),
            Route::Folder {
                account: "ed+work@acme.com".to_string(),
                folder: "Label_12".to_string(),
            },
            message("18f3a2c9b1"),
            Route::Reply {
                account: "ed@acme.com".to_string(),
                folder: "SENT".to_string(),
                id: "18f3a2c9b1".to_string(),
            },
            Route::New {
                account: "second@acme.com".to_string(),
                query: String::new(),
            },
            Route::New {
                account: "second@acme.com".to_string(),
                query: "to=ale@acme.com&subject=Hi%20there&attach=%2Ftmp%2Fq3.pdf".to_string(),
            },
        ] {
            assert_eq!(Route::parse(&route.to_string()), Some(route.clone()), "{route}");
        }
        assert_eq!(Route::inbox("ed@acme.com").to_string(), "/ed@acme.com/INBOX");
        assert_eq!(Route::parse("/ed@acme.com"), Some(Route::inbox("ed@acme.com")));
        assert_eq!(Route::parse("/ed@acme.com/INBOX/18f3/forward/x"), None);
        assert_eq!(Route::parse("ed@acme.com/INBOX"), None);
    }

    /// A search is a folder of its own, and what is opened from it remembers
    /// it: the path is `/:account/search/:query`, and back again.
    #[test]
    fn a_search_round_trips_as_a_folder() {
        let found = Route::Folder {
            account: "ed@acme.com".to_string(),
            folder: searching("from:ale invoice/2025"),
        };
        assert_eq!(found.to_string(), "/ed@acme.com/search/from%3Aale%20invoice%2F2025");
        assert_eq!(Route::parse(&found.to_string()), Some(found.clone()));
        assert_eq!(found.folder().and_then(query_of), Some("from:ale invoice/2025"));
        for route in [
            Route::Message {
                account: "ed@acme.com".to_string(),
                folder: searching("invoice"),
                id: "18f3".to_string(),
            },
            Route::Reply {
                account: "ed@acme.com".to_string(),
                folder: searching("invoice"),
                id: "18f3".to_string(),
            },
        ] {
            assert_eq!(Route::parse(&route.to_string()), Some(route.clone()), "{route}");
        }
        // Its parent is the search, so Back from a message goes to the results.
        let message = Route::parse("/ed@acme.com/search/invoice/18f3").expect("a message");
        assert_eq!(
            message.parent(),
            Some(Route::Folder {
                account: "ed@acme.com".to_string(),
                folder: searching("invoice"),
            })
        );
        assert_eq!(query_of(INBOX), None);
    }

    /// A forward hangs off the message it forwards, and an account's settings
    /// off the settings: both round-trip, and Back goes up one level.
    #[test]
    fn a_forward_and_an_accounts_settings_round_trip() {
        let forward = Route::Forward {
            account: "ed@acme.com".to_string(),
            folder: INBOX.to_string(),
            id: "18f3".to_string(),
        };
        assert_eq!(forward.to_string(), "/ed@acme.com/INBOX/18f3/forward");
        assert_eq!(Route::parse(&forward.to_string()), Some(forward.clone()));
        assert_eq!(forward.parent(), Some(message("18f3")));
        assert!(forward.writing());
        let found = Route::Forward {
            account: "ed@acme.com".to_string(),
            folder: searching("offert"),
            id: "18f3".to_string(),
        };
        assert_eq!(Route::parse(&found.to_string()), Some(found));

        let settings = Route::Account {
            account: "ed@acme.com".to_string(),
        };
        assert_eq!(settings.to_string(), "/settings/ed@acme.com");
        assert_eq!(Route::parse(&settings.to_string()), Some(settings.clone()));
        assert_eq!(settings.parent(), Some(Route::Settings));
        assert!(!settings.writing());
    }

    /// A draft is its own screen, `/:account/drafts/:draft` — not a message
    /// in the Drafts folder, which is `/:account/DRAFT/:id`.
    #[test]
    fn a_draft_is_not_a_message_in_the_drafts_folder() {
        let draft = Route::Draft {
            account: "ed@acme.com".to_string(),
            draft: "r-123".to_string(),
        };
        assert_eq!(draft.to_string(), "/ed@acme.com/drafts/r-123");
        assert_eq!(Route::parse(&draft.to_string()), Some(draft.clone()));
        assert!(draft.writing());
        assert_eq!(draft.parent(), None, "Back goes where it was opened from");
        assert_eq!(
            Route::parse("/ed@acme.com/DRAFT/18f3"),
            Some(Route::Message {
                account: "ed@acme.com".to_string(),
                folder: "DRAFT".to_string(),
                id: "18f3".to_string(),
            })
        );
    }

    /// A new email's query fills its fields the way a mailto link does.
    #[test]
    fn a_new_email_is_filled_in_from_its_query() {
        let query = query_of_mailto("mailto:ale@acme.com?subject=Hi%20there&attach=%2Ftmp%2Fq3.pdf");
        assert_eq!(query, "to=ale@acme.com&subject=Hi%20there&attach=%2Ftmp%2Fq3.pdf");
        let mail = Mailto::parse(&format!("mailto:?{query}"));
        assert_eq!(mail.to, "ale@acme.com");
        assert_eq!(mail.subject, "Hi there");
        assert_eq!(mail.attach, vec!["/tmp/q3.pdf".to_string()]);
    }

    /// A mailto link becomes its fields: addresses, subject, body, cc.
    #[test]
    fn a_mailto_link_is_read_into_its_fields() {
        let mail = Mailto::parse(
            "mailto:ale@acme.com?subject=Join%20Acme&body=Line%201%0D%0ALine%202&cc=cm@ey.com",
        );
        assert_eq!(mail.to, "ale@acme.com");
        assert_eq!(mail.subject, "Join Acme");
        assert_eq!(mail.body, "Line 1\nLine 2");
        assert_eq!(mail.cc, "cm@ey.com");
        assert_eq!(Mailto::parse("mailto:?subject=a+b").subject, "a+b");
    }

    /// The commands that open the window, and only those.
    #[test]
    fn the_command_line_names_the_route() {
        assert_eq!(
            Route::of_words(&words("list ed@acme.com")),
            Some(Route::inbox("ed@acme.com"))
        );
        assert_eq!(Route::of_words(&words("read ed@acme.com 18f3a2c9b1")), Some(message("18f3a2c9b1")));
        assert_eq!(
            Route::of_words(&words("reply ed@acme.com 18f3a2c9b1")),
            Some(Route::Reply {
                account: "ed@acme.com".to_string(),
                folder: INBOX.to_string(),
                id: "18f3a2c9b1".to_string(),
            })
        );
        assert_eq!(
            Route::of_words(&words("open /ed@acme.com/SENT/18f3")),
            Some(Route::Message {
                account: "ed@acme.com".to_string(),
                folder: "SENT".to_string(),
                id: "18f3".to_string(),
            })
        );
        assert_eq!(Route::of_words(&words("list --nope")), None);
    }

    /// A reply is not the message it answers: the message is behind it.
    #[test]
    fn composing_shows_a_reply_rather_than_the_message() {
        let reply = Route::Reply {
            account: "ed@acme.com".to_string(),
            folder: INBOX.to_string(),
            id: "18f3".to_string(),
        };
        assert_eq!(reply.account(), Some("ed@acme.com"));
        assert_eq!(reply.message(), None);
        assert_eq!(reply.replying_to(), Some("18f3"));
        assert_eq!(reply.folder(), Some(INBOX));
        assert_eq!(message("18f3").replying_to(), None);
    }

    /// Everything else stays in the terminal — the scripting verbs, the browser
    /// flow, the help, and anything misspelled.
    #[test]
    fn the_terminal_verbs_open_nothing() {
        for line in [
            "accounts",
            "add-account",
            "mark ed@acme.com 18f3 --read",
            "delete ed@acme.com 18f3",
            "attachment ed@acme.com 18f3 quote.pdf",
            "help",
            "--help",
            "-h",
            "sendmail",
            "draft ed@acme.com 18f3 hello",
            "send ed@acme.com reply-18f3",
            "list ed@acme.com --page tok3n",
        ] {
            assert_eq!(Route::of_words(&words(line)), None, "\"{line}\" opened a window");
        }
    }

    /// Back retraces the steps taken; replace swaps the top without a step.
    #[test]
    fn the_router_pushes_replaces_and_goes_back() {
        let mut screen = 0u8;
        let mut router = Router::of(Route::inbox("ed@acme.com"));
        router.push(message("a"), &mut screen);
        router.replace(message("b"), &mut screen);
        assert_eq!(router.top(), &message("b"));
        assert_eq!(router.depth(), 2, "picking another message is no new step");
        assert!(router.back(&mut screen));
        assert_eq!(router.top(), &Route::inbox("ed@acme.com"));
        assert!(!router.back(&mut screen), "the route the window opened at is the floor");
        router.push(message("a"), &mut screen);
        router.push(Route::Settings, &mut screen);
        router.push(Route::inbox("ed@acme.com"), &mut screen);
        assert_eq!(router.depth(), 1, "stepping onto a route behind us returns to it");
    }

    /// Back goes up one level: a message has its folder behind it and a
    /// reply its message, however the window got there.
    #[test]
    fn a_message_always_has_its_folder_behind_it() {
        let mut screen = 0u8;
        let reply = Route::Reply {
            account: "ed@acme.com".to_string(),
            folder: INBOX.to_string(),
            id: "a".to_string(),
        };
        let mut router: Router<u8> = Router::of(reply.clone());
        assert_eq!(router.depth(), 3, "opened straight at a reply");
        assert!(router.back(&mut screen));
        assert_eq!(router.top(), &message("a"));
        assert!(router.back(&mut screen));
        assert_eq!(router.top(), &Route::inbox("ed@acme.com"));

        // Walked to from somewhere else, the folder is stepped through.
        let mut router: Router<u8> = Router::of(Route::Settings);
        router.push(message("a"), &mut screen);
        assert_eq!(router.depth(), 3);
        assert_eq!(router.below_mut().map(|(_, route, _)| route.clone()), Some(Route::inbox("ed@acme.com")));

        // A folder already behind is returned to, and what was above it goes.
        let mut router: Router<u8> = Router::of(Route::inbox("ed@acme.com"));
        router.push(Route::Settings, &mut screen);
        router.push(message("b"), &mut screen);
        assert_eq!(router.depth(), 2, "settings was above the inbox, and is gone");
        assert_eq!(router.top(), &message("b"));
    }

    /// Every step keeps its screen: a new step starts empty, Back and a step
    /// onto a route behind us bring that step's screen back as it was left,
    /// and a step that is dropped takes its screen with it.
    #[test]
    fn every_step_keeps_its_own_screen() {
        let mut screen = 1u8;
        let mut router = Router::of(Route::inbox("ed@acme.com"));
        let inbox_key = router.key();

        router.push(message("a"), &mut screen);
        assert_eq!(screen, 0, "a new step starts empty");
        assert_ne!(router.key(), inbox_key, "under a key of its own");
        screen = 2;
        router.push(Route::Settings, &mut screen);
        screen = 3;

        // Every screen, the one on screen standing in for its own step.
        let all: Vec<(u64, Route, u8)> = router
            .screens(&mut screen)
            .map(|(key, route, screen)| (key, route.clone(), *screen))
            .collect();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0], (inbox_key, Route::inbox("ed@acme.com"), 1));
        assert_eq!((all[1].1.clone(), all[1].2), (message("a"), 2));
        assert_eq!((all[2].1.clone(), all[2].2), (Route::Settings, 3));

        assert!(router.back(&mut screen));
        assert_eq!(screen, 2, "back to the message as it was left");

        // Onto the inbox behind us: everything above it is dropped.
        router.push(Route::inbox("ed@acme.com"), &mut screen);
        assert_eq!(screen, 1);
        assert_eq!(router.key(), inbox_key);
        assert_eq!(router.depth(), 1);

        // Replace drops the screen on top and leaves the one behind alone.
        router.push(message("a"), &mut screen);
        screen = 4;
        router.replace(message("b"), &mut screen);
        assert_eq!(screen, 0, "the replaced step's screen went with it");
        assert!(router.back(&mut screen));
        assert_eq!(screen, 1, "the inbox kept its own");
    }
}
