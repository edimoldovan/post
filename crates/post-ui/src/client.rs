//! The only file that calls the mail library.
//!
//! Everything above this line works in plain types — [`Mailbox`], [`Row`],
//! [`Message`] — and never sees a `serde_json::Value`, exactly as Grido's UI
//! never sees IronCalc. What the window runs are the same verbs the assistant
//! and the terminal run: one palette, one implementation, so a message deleted
//! from a row and a message deleted from a script are the same call — and a
//! reply sent from the editor is the same call as a reply the assistant sends,
//! minus the dialog, because the key press that sent it was the consent.
//!
//! Mail is on the network and the window is not allowed to stop for it, so a
//! job runs on a thread of its own and its answer comes back down a channel.
//! The parsing is separate from the running, which is what lets it be tested
//! with no account, no token and no network.

use std::sync::mpsc::{channel, Receiver, Sender};

use serde_json::{json, Value};

/// One message's row in a listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    /// The conversation this message is on. Empty where the provider did not
    /// say, in which case the message is a conversation of its own.
    pub thread_id: String,
    pub from: String,
    /// Who it went to, as the header wrote it — what a conversation of this
    /// account's own is named by, since its sender is the reader.
    pub to: String,
    pub subject: String,
    pub date: String,
    pub unread: bool,
    /// True where this account wrote it. The provider's own flag: an alias
    /// sends under another address, which no From line would give away.
    pub sent: bool,
    /// The start of the message, as Gmail cut it. Empty on a page cached
    /// before rows carried it.
    pub snippet: String,
    /// The draft this message is, on a row of the Drafts folder: what opens
    /// it in the new-email screen. Empty everywhere else.
    pub draft_id: String,
}

impl Row {
    /// Which conversation this row belongs to: the thread the provider named,
    /// or the message itself where it named none.
    pub fn conversation(&self) -> &str {
        if self.thread_id.is_empty() {
            &self.id
        } else {
            &self.thread_id
        }
    }

    /// Whether the reader wrote this one: the provider said so, or the From
    /// line is the account itself.
    pub fn mine(&self, account: &str) -> bool {
        let account = account.trim();
        self.sent
            || (!account.is_empty()
                && self.from.to_lowercase().contains(&account.to_lowercase()))
    }
}

/// One conversation's line in the inbox: the newest message on it, the subject
/// they share, and how many there are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    /// The conversation's id — what `post_thread` is asked for.
    pub id: String,
    /// The newest message on it: who it is from and when it arrived.
    pub latest: Row,
    /// Whose conversation this is, as the line names it: the newest message on
    /// it the reader did not write — or, on one only the reader wrote, where
    /// it went.
    pub who: String,
    pub subject: String,
    /// True when any message on it is unread: a conversation with something
    /// new in it is new.
    pub unread: bool,
    pub count: usize,
    /// Every message on it, newest first — what a trashed row amends.
    pub ids: Vec<String>,
}

/// One page of one account's mail.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mailbox {
    pub account: String,
    /// The folder this is a page of: a label id, or a search.
    pub folder: String,
    pub rows: Vec<Row>,
    /// The rows as conversations, newest first — what the inbox draws. Kept
    /// beside the rows rather than instead of them: the page on disk is a page
    /// of messages, and a message marked or trashed is still amended by its own
    /// id. Rebuilt by [`Mailbox::regroup`] whenever the rows change.
    pub threads: Vec<Thread>,
    /// True when this page came off the disk rather than off the wire — the
    /// mail library hands the last one over at once and refreshes behind it,
    /// so a fresher page is on its way down [`Jobs::drain`].
    pub cached: bool,
    /// The token for the page after this one, when the mailbox has one. `None`
    /// is the end of the mailbox: there is nothing more to append.
    pub next_page: Option<String>,
}

impl Mailbox {
    /// A `post_list` answer as rows.
    pub fn of_answer(account: &str, answer: &Value) -> Mailbox {
        let rows = answer
            .get("messages")
            .and_then(Value::as_array)
            .map(|rows| rows.iter().map(row_of).collect())
            .unwrap_or_default();
        let mut mailbox = Mailbox {
            account: answer
                .get("account")
                .and_then(Value::as_str)
                .unwrap_or(account)
                .to_string(),
            folder: answer
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or(crate::route::INBOX)
                .to_string(),
            rows,
            threads: Vec::new(),
            cached: answer.get("cached").and_then(Value::as_bool) == Some(true),
            next_page: answer
                .get("next_page")
                .and_then(Value::as_str)
                .filter(|token| !token.trim().is_empty())
                .map(str::to_owned),
        };
        mailbox.regroup();
        mailbox
    }

    pub fn row(&self, id: &str) -> Option<&Row> {
        self.rows.iter().find(|row| row.id == id)
    }

    /// Sets one row's unread flag. Nothing to do when the row is not in this
    /// page — a message read from a command line need not be listed here.
    pub fn set_unread(&mut self, id: &str, unread: bool) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
            row.unread = unread;
            self.regroup();
        }
    }

    /// Drops one row, after it has gone to the trash.
    pub fn remove(&mut self, id: &str) {
        self.rows.retain(|row| row.id != id);
        self.regroup();
    }

    /// Adds the page after this one to the end of it, and takes on where the
    /// mailbox now continues. Rows already here are not added twice — a page
    /// boundary can shift under a mailbox that took a new message meanwhile.
    ///
    /// The page is appended and the whole list regrouped: a later page can
    /// carry an older message of a conversation already on screen, which is a
    /// higher count on a line that is already there rather than a second line.
    pub fn append(&mut self, more: Mailbox) {
        for row in more.rows {
            if !self.rows.iter().any(|have| have.id == row.id) {
                self.rows.push(row);
            }
        }
        self.next_page = more.next_page;
        self.regroup();
    }

    /// The rows as conversations: one line per thread, in the order the
    /// mailbox lists them, carrying the newest message on it.
    ///
    /// The rows arrive newest first, so the first row of a conversation is its
    /// newest one — that is the line, and its place in the list is the
    /// conversation's place.
    pub fn regroup(&mut self) {
        let mut threads: Vec<Thread> = Vec::new();
        for row in &self.rows {
            let id = row.conversation().to_string();
            match threads.iter_mut().find(|thread| thread.id == id) {
                Some(thread) => {
                    thread.count += 1;
                    thread.unread |= row.unread;
                    thread.ids.push(row.id.clone());
                }
                None => threads.push(Thread {
                    id,
                    who: String::new(),
                    subject: row.subject.clone(),
                    unread: row.unread,
                    count: 1,
                    ids: vec![row.id.clone()],
                    latest: row.clone(),
                }),
            }
        }
        for thread in &mut threads {
            thread.who = self.naming(thread);
        }
        self.threads = threads;
    }

    /// What one conversation's line is named by: the newest message on it
    /// somebody else wrote. A conversation of the reader's own — a reply he
    /// sent, and nothing back yet — is named by where it went instead, because
    /// naming it after himself says nothing.
    fn naming(&self, thread: &Thread) -> String {
        let theirs = self
            .rows
            .iter()
            .filter(|row| row.conversation() == thread.id)
            .find(|row| !row.mine(&self.account));
        match theirs {
            Some(row) => row.from.clone(),
            None if !thread.latest.to.trim().is_empty() => {
                format!("To {}", thread.latest.to.trim())
            }
            None => thread.latest.from.clone(),
        }
    }

    /// One conversation of this page, by its id.
    pub fn thread(&self, id: &str) -> Option<&Thread> {
        self.threads.iter().find(|thread| thread.id == id)
    }

    /// How many conversations have something unread on them — the count on the
    /// account row.
    pub fn unread(&self) -> usize {
        self.threads.iter().filter(|thread| thread.unread).count()
    }
}

/// One file on a message, as its card shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub bytes: u64,
}

/// One message, as the detail view shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub account: String,
    pub from: String,
    pub subject: String,
    pub date: String,
    /// The body as text: what a plain-text message says, and what an HTML one
    /// says once it is stripped. Shown when there is no HTML to render.
    pub text: String,
    /// The saved HTML body, written by the mail library under the state dir.
    pub html_path: Option<String>,
    pub attachments: Vec<Attachment>,
    /// True where this message was still unread when the conversation was
    /// opened — what puts weight on its line.
    pub unread: bool,
}

impl Message {
    /// A `post_read` or `post_thread` answer as a message.
    pub fn of_answer(account: &str, answer: &Value) -> Message {
        let text = |key: &str| {
            answer
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Message {
            id: text("id"),
            account: account.to_string(),
            from: text("from"),
            subject: text("subject"),
            date: text("date"),
            text: text("text"),
            html_path: answer
                .get("html_path")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
                .map(str::to_owned),
            attachments: answer
                .get("attachments")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|a| {
                            Some(Attachment {
                                name: a.get("filename")?.as_str()?.to_owned(),
                                mime: a
                                    .get("mime")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                                bytes: a.get("bytes").and_then(Value::as_u64).unwrap_or(0),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            unread: answer.get("unread").and_then(Value::as_bool) == Some(true),
        }
    }
}

/// One conversation, as the detail view shows it: every message on it, oldest
/// first, with one of them open.
///
/// The keyboard walks the messages and Enter opens the one it is on; opening
/// one closes whatever was open, so the pane below always has exactly the
/// message on screen to draw — or nothing, with everything collapsed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conversation {
    pub account: String,
    pub id: String,
    pub subject: String,
    /// Oldest first, newest at the bottom — the order a conversation reads in.
    pub messages: Vec<Message>,
    /// Which message the keyboard is on.
    pub focus: usize,
    /// Which message is open, if any. The newest one, on arrival.
    pub open: Option<usize>,
}

impl Conversation {
    /// A `post_thread` answer as a conversation, opened at its newest message.
    pub fn of_answer(account: &str, answer: &Value) -> Conversation {
        let messages: Vec<Message> = answer
            .get("messages")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .map(|message| Message::of_answer(account, message))
                    .collect()
            })
            .unwrap_or_default();
        let newest = messages.len().saturating_sub(1);
        Conversation {
            account: account.to_string(),
            id: answer
                .get("thread_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            subject: answer
                .get("subject")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            focus: newest,
            open: (!messages.is_empty()).then_some(newest),
            messages,
        }
    }

    /// The message the keyboard is on.
    pub fn focused(&self) -> Option<&Message> {
        self.messages.get(self.focus)
    }

    /// The message that is open, whose body is on screen.
    pub fn opened(&self) -> Option<&Message> {
        self.messages.get(self.open?)
    }

    /// The newest message — what Reply answers.
    pub fn newest(&self) -> Option<&Message> {
        self.messages.last()
    }

    /// Moves the keyboard by `by` messages, stopping at either end. True when
    /// it moved, so the list knows to scroll after it.
    pub fn step(&mut self, by: i32) -> bool {
        if self.messages.is_empty() {
            return false;
        }
        let last = self.messages.len() as i32 - 1;
        let next = (self.focus as i32 + by).clamp(0, last) as usize;
        if next == self.focus {
            return false;
        }
        self.focus = next;
        true
    }

    /// Enter: opens the message the keyboard is on, or closes it when it is
    /// already the open one. Only ever one is open.
    pub fn toggle(&mut self) {
        if self.messages.is_empty() {
            return;
        }
        self.open = match self.open {
            Some(open) if open == self.focus => None,
            _ => Some(self.focus),
        };
    }

    /// Drops one message, after it has gone to the trash, and keeps the
    /// keyboard and the open message on something that is still here.
    pub fn remove(&mut self, id: &str) {
        let Some(at) = self.messages.iter().position(|m| m.id == id) else {
            return;
        };
        self.messages.remove(at);
        if self.messages.is_empty() {
            self.focus = 0;
            self.open = None;
            return;
        }
        let last = self.messages.len() - 1;
        self.focus = self.focus.min(last);
        self.open = match self.open {
            Some(open) if open == at => Some(self.focus),
            Some(open) if open > at => Some(open - 1),
            other => other,
        };
    }
}

/// A reply, written and not yet sent — what the compose view puts above the
/// editor. The body is not here: the editor owns it until it is sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub id: String,
    /// Who it goes to and who is copied, comma separated as the fields show
    /// them. A reply answers everybody, and both lines are the reader's.
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    /// What it says. A reply's carries the quote under what was typed.
    pub body: String,
    /// Its files, absolute paths — where a draft read back fetched them to.
    pub attachments: Vec<String>,
    /// "On <date>, <sender> wrote:" — the line above the quote, as the mail
    /// library wrote it into the reply.
    pub attribution: String,
    /// The message being answered, as its own text: what the compose view
    /// shows under the editor, unquoted and not editable. The sent body
    /// carries the same words behind `> `.
    pub quote: String,
}

impl Draft {
    /// A `post_draft` answer as the header of the reply being written.
    pub fn of_answer(answer: &Value) -> Draft {
        let text = |key: &str| {
            answer
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Draft {
            id: text("draft_id"),
            to: text("to"),
            cc: text("cc"),
            bcc: text("bcc"),
            subject: text("subject"),
            body: text("body"),
            attachments: answer
                .get("attachments")
                .and_then(Value::as_array)
                .map(|files| {
                    files
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            attribution: text("attribution"),
            quote: text("original_text"),
        }
    }
}

fn row_of(row: &Value) -> Row {
    let text = |key: &str| {
        row.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Row {
        id: text("id"),
        thread_id: text("thread_id"),
        from: text("from"),
        to: text("to"),
        subject: text("subject"),
        date: text("date"),
        unread: row.get("unread").and_then(Value::as_bool) == Some(true),
        sent: row.get("sent").and_then(Value::as_bool) == Some(true),
        snippet: text("snippet"),
        draft_id: text("draft_id"),
    }
}

/// A page the mail library refreshed on its own, as the thing the window
/// keeps. A refresh that failed says so rather than leaving a cached page to
/// look like a fresh one.
fn refreshed(page: &Value) -> Done {
    match page.get("error").and_then(Value::as_str) {
        Some(error) => Done::Failed {
            about: "Refreshing the mailbox:".to_string(),
            error: error.to_string(),
        },
        None => Done::List(Mailbox::of_answer("", page)),
    }
}

/// Something to ask the mail library for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Job {
    Accounts,
    /// One page of a folder — a label id, or a search — and with `unread` only
    /// what is unread in it. `page` is a token from the page before it, which
    /// makes this an append rather than the mailbox itself.
    List {
        account: String,
        folder: String,
        unread: bool,
        page: Option<String>,
    },
    /// An account's folders, with their unread counts.
    Labels { account: String },
    /// One conversation, every message on it. `id` is the thread's, and a
    /// message id works too — a window opened at `post read <email>
    /// <id>` has one of those and nothing else.
    Thread { account: String, id: String },
    Mark { account: String, id: String, read: bool },
    /// Save one attachment of a message and hand back where it landed:
    /// `path` is where the reader chose (Save as…), `open` hands it to its
    /// app afterwards.
    Attachment { account: String, id: String, name: String, path: Option<String>, open: bool },
    Delete { account: String, id: String },
    /// Write a new message into Gmail's Drafts — not a reply — over
    /// `draft_id` when there is one. Free, and nothing is sent.
    New {
        account: String,
        to: String,
        cc: String,
        bcc: String,
        subject: String,
        body: String,
        attach: Vec<String>,
        draft_id: Option<String>,
    },
    /// Write the reply to one message into Gmail's Drafts, over `draft_id`
    /// when there is one. Free, and nothing is sent: the window asks once with
    /// nothing in it to read the headers and the quote off — which writes
    /// nothing — and again as the reader types.
    ///
    /// `to`, `cc` and `bcc` are the fields as they stand. The first ask leaves
    /// them out, which is what fills them in: the reply-all off the message
    /// being answered.
    Draft {
        account: String,
        id: String,
        body: String,
        attach: Vec<String>,
        to: Option<String>,
        cc: Option<String>,
        bcc: Option<String>,
        draft_id: Option<String>,
    },
    /// A `Draft` or a `New` written as the reader types, quietly: what comes
    /// back is [`Done::Written`].
    Save(Box<Job>),
    /// One Gmail draft, to write on: its fields, and its files fetched.
    ReadDraft { account: String, draft_id: String },
    /// A Gmail draft deleted for good.
    DeleteDraft { account: String, draft_id: String },
    /// Forward one message into Gmail's Drafts, over `draft_id` when there is
    /// one. The window asks once with nothing in it to read the header, the
    /// subject and the files off — which writes nothing — and again as the
    /// reader types; `attach` left out is the message's own files.
    Forward {
        account: String,
        id: String,
        body: String,
        subject: Option<String>,
        to: Option<String>,
        cc: Option<String>,
        bcc: Option<String>,
        attach: Option<Vec<String>>,
        draft_id: Option<String>,
    },
    /// The name an account goes by.
    Rename { account: String, name: String },
    /// An account off this computer: its sign-in, its caches, its entry.
    Remove { account: String },
    /// Send a draft already written. No dialog on this road: the click or the
    /// key press that got here *is* the consent.
    Send { account: String, draft_id: String },
    /// Write down what was typed — a `Draft` or a `New` — and send that draft.
    /// Two calls, one errand: it goes whatever the window does meanwhile.
    Deliver(Box<Job>),
    AddAccount,
}

impl Job {
    /// What to say while this is in flight.
    pub fn about(&self) -> String {
        match self {
            Job::Accounts => "Reading the account list…".to_string(),
            Job::List { account, page, .. } => match page {
                Some(_) => "Fetching more…".to_string(),
                None => format!("Fetching {account}…"),
            },
            // The sidebar's counts: kept up to date behind the screen, quietly.
            Job::Labels { .. } => String::new(),
            Job::Thread { .. } => "Opening the conversation…".to_string(),
            Job::Mark { .. } => "Marking…".to_string(),
            Job::Delete { .. } => "Moving to the trash…".to_string(),
            Job::Draft { .. } => "Preparing the reply…".to_string(),
            Job::New { .. } => "Preparing the message…".to_string(),
            Job::Attachment { name, .. } => format!("Saving {name}…"),
            Job::Send { .. } | Job::Deliver(_) => "Sending…".to_string(),
            // Saving as the reader types says so in the editor, not down here.
            Job::Save(_) => String::new(),
            Job::ReadDraft { .. } => "Opening the draft…".to_string(),
            Job::Forward { .. } => "Preparing the forward…".to_string(),
            Job::Rename { .. } => "Renaming…".to_string(),
            Job::Remove { account } => format!("Removing {account}…"),
            Job::DeleteDraft { .. } => "Discarding the draft…".to_string(),
            Job::AddAccount => {
                "Waiting for Google's sign-in in your browser…".to_string()
            }
        }
    }

    /// The verb and its arguments: the same call the assistant and the terminal
    /// make.
    fn verb(&self) -> (&'static str, Value) {
        match self {
            Job::Accounts => ("post_accounts", json!({})),
            Job::List {
                account,
                folder,
                unread,
                page,
            } => {
                let mut args = json!({ "email": account });
                let unread = unread.then_some("is:unread");
                match crate::route::query_of(folder) {
                    Some(query) => {
                        args["query"] = json!(match unread {
                            Some(filter) => format!("{query} {filter}"),
                            None => query.to_string(),
                        });
                    }
                    None => {
                        args["label"] = json!(folder);
                        if let Some(filter) = unread {
                            args["query"] = json!(filter);
                        }
                    }
                }
                if let Some(page) = page {
                    args["page"] = json!(page);
                }
                ("post_list", args)
            }
            Job::Labels { account } => ("post_labels", json!({ "email": account })),
            Job::Thread { account, id } => (
                "post_thread",
                json!({"email": account, "thread_id": id}),
            ),
            Job::Mark { account, id, read } => (
                "post_mark",
                json!({"email": account, "id": id, "read": read}),
            ),
            Job::Delete { account, id } => ("post_delete", json!({"email": account, "id": id})),
            Job::Attachment { account, id, name, .. } => (
                "post_attachment",
                json!({"email": account, "id": id, "name": name}),
            ),
            Job::New {
                account,
                to,
                cc,
                bcc,
                subject,
                body,
                attach,
                draft_id,
            } => {
                let mut args = json!({
                    "email": account,
                    "to": to,
                    "cc": cc,
                    "bcc": bcc,
                    "subject": subject,
                    "body": body,
                });
                if !attach.is_empty() {
                    args["attach"] = json!(attach.join(","));
                }
                if let Some(draft_id) = draft_id {
                    args["draft_id"] = json!(draft_id);
                }
                ("post_new", args)
            }
            Job::Draft {
                account,
                id,
                body,
                attach,
                to,
                cc,
                bcc,
                draft_id,
            } => {
                let mut args = json!({"email": account, "id": id, "body": body});
                if !attach.is_empty() {
                    args["attach"] = json!(attach.join(","));
                }
                for (key, value) in [("to", to), ("cc", cc), ("bcc", bcc), ("draft_id", draft_id)] {
                    if let Some(value) = value {
                        args[key] = json!(value);
                    }
                }
                ("post_draft", args)
            }
            Job::Save(write) => write.verb(),
            Job::Forward {
                account,
                id,
                body,
                subject,
                to,
                cc,
                bcc,
                attach,
                draft_id,
            } => {
                let mut args = json!({"email": account, "id": id, "body": body});
                for (key, value) in [
                    ("subject", subject),
                    ("to", to),
                    ("cc", cc),
                    ("bcc", bcc),
                    ("draft_id", draft_id),
                ] {
                    if let Some(value) = value {
                        args[key] = json!(value);
                    }
                }
                if let Some(attach) = attach {
                    args["attach"] = json!(attach.join(","));
                }
                ("post_forward", args)
            }
            Job::Rename { account, name } => (
                "post_rename",
                json!({"email": account, "name": name}),
            ),
            Job::Remove { account } => ("post_remove", json!({ "email": account })),
            Job::ReadDraft { account, draft_id } => (
                "post_draft_read",
                json!({"email": account, "draft_id": draft_id}),
            ),
            Job::DeleteDraft { account, draft_id } => (
                "post_draft_delete",
                json!({"email": account, "draft_id": draft_id}),
            ),
            Job::Send { account, draft_id } => (
                "post_send",
                json!({"email": account, "draft_id": draft_id}),
            ),
            Job::Deliver(write) => write.verb(),
            Job::AddAccount => ("post_add_account", json!({})),
        }
    }

    /// Runs it. Blocking — this is what the thread is for.
    pub fn run(self) -> Done {
        let write = match self {
            Job::Deliver(write) => write,
            Job::Save(write) => {
                return match write.call() {
                    Done::Drafted(draft) => Done::Written(draft),
                    other => other,
                };
            }
            job => return job.call(),
        };
        let account = match write.as_ref() {
            Job::Draft { account, .. } | Job::New { account, .. } | Job::Forward { account, .. } => {
                account.clone()
            }
            _ => String::new(),
        };
        match write.call() {
            Done::Drafted(draft) => Job::Send {
                account,
                draft_id: draft.id,
            }
            .call(),
            other => other,
        }
    }

    /// One verb, called.
    fn call(self) -> Done {
        let (tool, args) = self.verb();
        match post::verbs::call(tool, &args) {
            Ok(answer) => self.answered(&answer),
            Err(error) => Done::Failed {
                about: self.about(),
                error,
            },
        }
    }

    /// One verb's answer, as the thing the window keeps.
    fn answered(self, answer: &Value) -> Done {
        match self {
            Job::Accounts => Done::Accounts(
                answer
                    .get("accounts")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(|a| {
                                let email = a.get("email").and_then(Value::as_str)?;
                                let name = a.get("name").and_then(Value::as_str).unwrap_or(email);
                                Some((email.to_owned(), name.to_owned()))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            Job::Rename { account, .. } => Done::Renamed {
                name: answer
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&account)
                    .to_string(),
                account,
            },
            Job::Remove { account } => Done::Removed(account),
            Job::List {
                account,
                folder,
                page,
                ..
            } => {
                let mut mailbox = Mailbox::of_answer(&account, answer);
                mailbox.folder = folder;
                match page {
                    Some(_) => Done::More(mailbox),
                    None => Done::List(mailbox),
                }
            }
            Job::Labels { account } => Done::Labels {
                account,
                labels: answer
                    .get("labels")
                    .and_then(Value::as_array)
                    .map(|labels| labels.iter().filter_map(Label::of_answer).collect())
                    .unwrap_or_default(),
            },
            Job::Thread { account, .. } => {
                Done::Thread(Conversation::of_answer(&account, answer))
            }
            Job::Mark { id, read, .. } => Done::Marked { id, read },
            Job::Attachment { path, open, name, .. } => {
                let saved = answer
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                // Save as: the library saves where it always does, and the
                // file moves to where the reader chose — the agent's verb
                // never gets to write to arbitrary paths.
                match path {
                    None => Done::Saved { path: saved, open },
                    Some(chosen) => match move_file(&saved, &chosen) {
                        Ok(()) => Done::Saved { path: chosen, open },
                        Err(error) => Done::Failed {
                            about: format!("Saving {name}…"),
                            error,
                        },
                    },
                }
            }
            Job::Delete { id, .. } => Done::Deleted { id },
            Job::Draft { .. } | Job::New { .. } | Job::Forward { .. } => {
                Done::Drafted(Draft::of_answer(answer))
            }
            Job::Send { .. } => Done::Sent {
                to: answer
                    .get("to")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
            Job::Deliver(write) | Job::Save(write) => write.answered(answer),
            Job::ReadDraft { .. } => Done::Opened(Draft::of_answer(answer)),
            Job::DeleteDraft { draft_id, .. } => Done::Discarded { draft_id },
            Job::AddAccount => Done::Added(
                answer
                    .get("email")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            ),
        }
    }
}

/// One folder of an account, as the sidebar lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    /// Gmail's label id: what a folder's route carries.
    pub id: String,
    /// As Gmail shows it; a nested label is `Parent/Child`.
    pub name: String,
    pub system: bool,
    /// How many of its conversations are unread.
    pub unread: u64,
    /// How many messages it holds — what Drafts counts, its drafts being
    /// none of them unread.
    pub total: u64,
}

impl Label {
    fn of_answer(label: &Value) -> Option<Label> {
        Some(Label {
            id: label.get("id")?.as_str()?.to_string(),
            name: label.get("name")?.as_str()?.to_string(),
            system: label.get("system").and_then(Value::as_bool) == Some(true),
            unread: label.get("unread").and_then(Value::as_u64).unwrap_or(0),
            total: label.get("total").and_then(Value::as_u64).unwrap_or(0),
        })
    }
}

/// What came back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Done {
    /// The connected accounts, each with the name it goes by.
    Accounts(Vec<(String, String)>),
    /// An account goes by this name now — its address when it has none.
    Renamed { account: String, name: String },
    /// An account taken off this computer.
    Removed(String),
    /// An account's folders, with their unread counts.
    Labels { account: String, labels: Vec<Label> },
    List(Mailbox),
    /// The page after the one on screen, to go on the end of it.
    More(Mailbox),
    /// The conversation on screen, with its newest message open.
    Thread(Conversation),
    Marked { id: String, read: bool },
    Deleted { id: String },
    /// An attachment landed on disk, at this path; `open` hands it to its app.
    Saved { path: String, open: bool },
    /// The reply as it now stands, addressed and threaded.
    Drafted(Draft),
    /// What was typed, written into Gmail's Drafts under the id this carries.
    Written(Draft),
    /// A Gmail draft, read to be written on.
    Opened(Draft),
    /// A Gmail draft, gone for good.
    Discarded { draft_id: String },
    /// It left. Nothing comes back to read: the mailbox has it now.
    Sent { to: String },
    Added(String),
    Failed { about: String, error: String },
}

/// The jobs in flight, and the channel their answers come back on.
pub struct Jobs {
    /// Answers, each with the key of the step whose call it was.
    tx: Sender<(u64, Done)>,
    rx: Receiver<(u64, Done)>,
    /// Pages the mail library refreshed on its own, after handing over a
    /// cached one. Nobody asked for these, so they arrive on a channel of
    /// their own rather than as a job's answer.
    fresh: Receiver<Value>,
    running: usize,
    /// True between a cached page and the fresh one that follows it, so the
    /// window keeps ticking until the correction lands.
    awaiting: bool,
}

impl Default for Jobs {
    fn default() -> Jobs {
        Jobs::new()
    }
}

impl Jobs {
    pub fn new() -> Jobs {
        let (tx, rx) = channel();
        Jobs {
            tx,
            rx,
            // Subscribing is also what lets the mail library answer from its
            // cache: it only does that where something is listening for the
            // refresh, and this window is that something.
            fresh: post::gmail::subscribe(),
            running: 0,
            awaiting: false,
        }
    }

    /// True while anything is in flight — a job, or a refresh behind a cached
    /// page.
    pub fn busy(&self) -> bool {
        self.running > 0 || self.awaiting
    }

    /// Starts a job on a thread of its own, for the step `key`. `wake` is the
    /// window's repaint handle, so an answer lands on screen without the mouse
    /// having to move.
    pub fn start(&mut self, key: u64, job: Job, wake: Option<eframe::egui::Context>) {
        let tx = self.tx.clone();
        self.running += 1;
        let started = std::thread::Builder::new()
            .name("post-job".into())
            .spawn(move || {
                let done = job.run();
                let _ = tx.send((key, done));
                if let Some(ctx) = wake {
                    ctx.request_repaint();
                }
            });
        if started.is_err() {
            self.running -= 1;
            let _ = self.tx.send((
                key,
                Done::Failed {
                    about: "Starting the call".to_string(),
                    error: "this computer would not start another thread".to_string(),
                },
            ));
        }
    }

    /// Everything that has come back since the last frame: the jobs that
    /// finished, each with its step's key, and then any page that refreshed
    /// itself behind a cached one, which belongs to no step in particular.
    pub fn drain(&mut self) -> Vec<(Option<u64>, Done)> {
        let mut done = Vec::new();
        while let Ok((key, answer)) = self.rx.try_recv() {
            self.running = self.running.saturating_sub(1);
            if let Done::List(mailbox) = &answer
                && mailbox.cached
            {
                self.awaiting = true;
            }
            done.push((Some(key), answer));
        }
        while let Ok(page) = self.fresh.try_recv() {
            self.awaiting = false;
            done.push((None, refreshed(&page)));
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listing exactly as `post_list` answers it.
    fn listing() -> Value {
        json!({
            "account": "ed@acme.com",
            "messages": [
                {"id": "18f3", "thread_id": "t-quote", "from": "Ale <ale@acme.com>",
                 "subject": "Re: the quote", "date": "2025-09-12 10:33", "unread": true},
                {"id": "18f2", "thread_id": "t-slides", "from": "EY <cristiano@ey.com>",
                 "subject": "Slides", "date": "2025-09-11 08:02", "unread": false}
            ],
            "next_page": "tok3n"
        })
    }

    #[test]
    fn a_listing_becomes_rows() {
        let mailbox = Mailbox::of_answer("ed@acme.com", &listing());
        assert_eq!(mailbox.account, "ed@acme.com");
        assert_eq!(mailbox.rows.len(), 2);
        assert_eq!(mailbox.rows[0].subject, "Re: the quote");
        assert_eq!(mailbox.rows[0].date, "2025-09-12 10:33");
        assert!(mailbox.rows[0].unread);
        assert!(!mailbox.rows[1].unread);
        assert_eq!(mailbox.unread(), 1);
        assert_eq!(mailbox.next_page.as_deref(), Some("tok3n"));

        // An empty mailbox is an answer, not a failure.
        let empty = Mailbox::of_answer("ed@acme.com", &json!({"messages": []}));
        assert!(empty.rows.is_empty());
        assert_eq!(empty.account, "ed@acme.com");
        assert_eq!(empty.next_page, None, "the end of a mailbox has no token");
    }

    /// The page after this one goes on the end, selection and all — and a row
    /// that is somehow in both pages is still only in the list once.
    #[test]
    fn a_later_page_goes_on_the_end() {
        let mut mailbox = Mailbox::of_answer("ed@acme.com", &listing());
        let more = Mailbox::of_answer(
            "ed@acme.com",
            &json!({
                "account": "ed@acme.com",
                "messages": [
                    {"id": "18f2", "from": "EY <cristiano@ey.com>", "subject": "Slides",
                     "date": "2025-09-11 08:02", "unread": false},
                    {"id": "18f1", "from": "Fredrik <fredrik@engelbrektsloppet.se>",
                     "subject": "Vinterveckan", "date": "2025-09-10 19:20", "unread": true}
                ]
            }),
        );
        mailbox.append(more);
        let ids: Vec<&str> = mailbox.rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, vec!["18f3", "18f2", "18f1"]);
        assert_eq!(mailbox.next_page, None, "and that was the last page");
    }

    /// The unread flag is the whole of "bold": rows carry it, and marking a
    /// message read clears it in place.
    #[test]
    fn marking_a_row_read_clears_its_weight() {
        let mut mailbox = Mailbox::of_answer("ed@acme.com", &listing());
        assert!(mailbox.row("18f3").expect("a row").unread);

        mailbox.set_unread("18f3", false);
        assert!(!mailbox.row("18f3").expect("a row").unread);
        assert_eq!(mailbox.unread(), 0);

        mailbox.set_unread("18f3", true);
        assert!(mailbox.row("18f3").expect("a row").unread);

        // A message this page has never heard of changes nothing.
        mailbox.set_unread("nope", false);
        assert_eq!(mailbox.rows.len(), 2);
    }

    #[test]
    fn a_trashed_row_leaves_the_page() {
        let mut mailbox = Mailbox::of_answer("ed@acme.com", &listing());
        mailbox.remove("18f2");
        assert_eq!(mailbox.rows.len(), 1);
        assert!(mailbox.row("18f2").is_none());
        mailbox.remove("18f2");
        assert_eq!(mailbox.rows.len(), 1, "removing it twice is not an error");
    }

    #[test]
    fn a_read_message_carries_its_body_and_its_html() {
        let answer = json!({
            "id": "18f3",
            "from": "Ale <ale@acme.com>",
            "date": "2025-09-12 10:33",
            "subject": "Re: the quote",
            "text": "Looks good — send it.",
            "html_path": "/home/ed/.local/share/armor/postbodies/ed@acme.com/18f3.html",
            "unread": false,
            "marked_read": true,
            "attachments": [{"filename": "quote.pdf", "mime": "application/pdf", "bytes": 8412}]
        });
        let message = Message::of_answer("ed@acme.com", &answer);
        assert_eq!(message.id, "18f3");
        assert_eq!(message.account, "ed@acme.com");
        assert_eq!(message.subject, "Re: the quote");
        assert_eq!(message.text, "Looks good — send it.");
        assert_eq!(
            message.attachments,
            vec![Attachment {
                name: "quote.pdf".to_string(),
                mime: "application/pdf".to_string(),
                bytes: 8412,
            }]
        );
        assert!(message.html_path.is_some());

        // A plain-text message has no HTML, and says so rather than pointing at
        // a file that is not there.
        let plain = Message::of_answer(
            "ed@acme.com",
            &json!({"id": "18f2", "text": "hi", "html_path": null}),
        );
        assert_eq!(plain.html_path, None);
        assert!(plain.attachments.is_empty());
    }

    /// A folder is a label or a search, and Unread narrows either: the list
    /// asks the mail library for exactly that.
    #[test]
    fn a_list_asks_for_its_folder() {
        let args = |folder: &str, unread: bool| {
            Job::List {
                account: "ed@acme.com".to_string(),
                folder: folder.to_string(),
                unread,
                page: None,
            }
            .verb()
            .1
        };
        assert_eq!(args("INBOX", false), json!({"email": "ed@acme.com", "label": "INBOX"}));
        assert_eq!(
            args("Label_12", true),
            json!({"email": "ed@acme.com", "label": "Label_12", "query": "is:unread"})
        );
        assert_eq!(
            args(&crate::route::searching("from:ale"), true),
            json!({"email": "ed@acme.com", "query": "from:ale is:unread"})
        );
    }

    /// Every job maps onto a verb from the palette, by its MCP name — the
    /// window adds no mail surface of its own.
    #[test]
    fn every_job_is_one_of_the_palettes_verbs() {
        let jobs = [
            Job::Accounts,
            Job::List {
                account: "ed@acme.com".to_string(),
                folder: crate::route::INBOX.to_string(),
                unread: false,
                page: None,
            },
            Job::List {
                account: "ed@acme.com".to_string(),
                folder: "Label_12".to_string(),
                unread: true,
                page: Some("tok3n".to_string()),
            },
            Job::List {
                account: "ed@acme.com".to_string(),
                folder: crate::route::searching("from:ale"),
                unread: false,
                page: None,
            },
            Job::Labels {
                account: "ed@acme.com".to_string(),
            },
            Job::Thread {
                account: "ed@acme.com".to_string(),
                id: "t-quote".to_string(),
            },
            Job::Mark {
                account: "ed@acme.com".to_string(),
                id: "18f3".to_string(),
                read: false,
            },
            Job::Delete {
                account: "ed@acme.com".to_string(),
                id: "18f3".to_string(),
            },
            Job::Draft {
                account: "ed@acme.com".to_string(),
                id: "18f3".to_string(),
                body: "Looks good.".to_string(),
                attach: Vec::new(),
                to: None,
                cc: None,
                bcc: None,
                draft_id: None,
            },
            Job::Send {
                account: "ed@acme.com".to_string(),
                draft_id: "reply-18f3".to_string(),
            },
            Job::AddAccount,
        ];
        for job in jobs {
            let (tool, args) = job.verb();
            assert!(
                post::verbs::find(tool).is_some(),
                "{tool} is not on the palette"
            );
            // Every job says what it is doing, but the sidebar's counts, which
            // are kept up to date quietly.
            assert_eq!(job.about().is_empty(), matches!(job, Job::Labels { .. }));
            // The arguments are the ones the verb asks for, checked by the
            // palette itself rather than by anything here.
            if tool != "post_accounts" && tool != "post_add_account" {
                assert!(args.get("email").is_some(), "{tool} lost its account");
            }
        }
    }

    /// The answers become state without a network anywhere near them.
    #[test]
    fn an_answer_becomes_what_the_window_keeps() {
        let accounts = Job::Accounts.answered(&json!({
            "accounts": [{"email": "ed@acme.com"}, {"email": "ale@acme.com"}]
        }));
        assert_eq!(
            accounts,
            Done::Accounts(vec![
                ("ed@acme.com".to_string(), "ed@acme.com".to_string()),
                ("ale@acme.com".to_string(), "ale@acme.com".to_string()),
            ])
        );

        let listed = Job::List {
            account: "ed@acme.com".to_string(),
            folder: crate::route::INBOX.to_string(),
            unread: false,
            page: None,
        }
        .answered(&listing());
        assert!(matches!(listed, Done::List(mailbox) if mailbox.rows.len() == 2));

        // The same answer asked for with a token is the next page, not the
        // mailbox: it goes on the end rather than replacing what is there.
        let more = Job::List {
            account: "ed@acme.com".to_string(),
            folder: crate::route::INBOX.to_string(),
            unread: false,
            page: Some("tok3n".to_string()),
        }
        .answered(&listing());
        assert!(matches!(more, Done::More(mailbox) if mailbox.rows.len() == 2));

        let marked = Job::Mark {
            account: "ed@acme.com".to_string(),
            id: "18f3".to_string(),
            read: false,
        }
        .answered(&json!({"id": "18f3", "read": false}));
        assert_eq!(
            marked,
            Done::Marked {
                id: "18f3".to_string(),
                read: false
            }
        );

        let deleted = Job::Delete {
            account: "ed@acme.com".to_string(),
            id: "18f3".to_string(),
        }
        .answered(&json!({"id": "18f3", "trashed": true}));
        assert_eq!(
            deleted,
            Done::Deleted {
                id: "18f3".to_string()
            }
        );

        let added = Job::AddAccount.answered(&json!({"email": "ed@acme.com", "connected": true}));
        assert_eq!(added, Done::Added("ed@acme.com".to_string()));
    }

    /// A reply written and a reply sent: the first comes back addressed, the
    /// second comes back as where it went and nothing more.
    #[test]
    fn a_reply_comes_back_addressed_and_then_gone() {
        let drafted = Job::Draft {
            account: "ed@acme.com".to_string(),
            id: "18f3".to_string(),
            body: "Looks good.".to_string(),
            attach: Vec::new(),
            to: None,
            cc: None,
            bcc: None,
            draft_id: None,
        }
        .answered(&json!({
            "draft_id": "reply-18f3",
            "account": "ed@acme.com",
            "to": "ale@acme.com",
            "cc": "cm@ey.com, fredrik@norberg.se",
            "subject": "Re: the quote",
            "body": "Looks good.\n\nOn 2025-09-12 10:33, Ale <ale@acme.com> wrote:\n> Send it?",
            "attribution": "On 2025-09-12 10:33, Ale <ale@acme.com> wrote:",
            "original_text": "Send it?",
            "sent": false,
        }));
        assert_eq!(
            drafted,
            Done::Drafted(Draft {
                id: "reply-18f3".to_string(),
                to: "ale@acme.com".to_string(),
                cc: "cm@ey.com, fredrik@norberg.se".to_string(),
                subject: "Re: the quote".to_string(),
                body: "Looks good.\n\nOn 2025-09-12 10:33, Ale <ale@acme.com> wrote:\n> Send it?"
                    .to_string(),
                attribution: "On 2025-09-12 10:33, Ale <ale@acme.com> wrote:".to_string(),
                quote: "Send it?".to_string(),
                ..Default::default()
            })
        );

        let sent = Job::Send {
            account: "ed@acme.com".to_string(),
            draft_id: "reply-18f3".to_string(),
        }
        .answered(&json!({"to": "ale@acme.com", "id": "18f4", "sent": true}));
        assert_eq!(
            sent,
            Done::Sent {
                to: "ale@acme.com".to_string()
            }
        );

        // An answer with nothing in it is not a panic.
        assert_eq!(Draft::of_answer(&json!({})), Draft::default());
    }

    /// The first draft asks for nothing but the headers; the one on the way out
    /// carries the fields as the reader left them, an emptied Cc included.
    #[test]
    fn the_address_fields_travel_with_the_draft() {
        let (_, opening) = Job::Draft {
            account: "ed@acme.com".to_string(),
            id: "18f3".to_string(),
            body: String::new(),
            attach: Vec::new(),
            to: None,
            cc: None,
            bcc: None,
            draft_id: None,
        }
        .verb();
        assert!(opening.get("to").is_none() && opening.get("cc").is_none());

        let (tool, sending) = Job::Draft {
            account: "ed@acme.com".to_string(),
            id: "18f3".to_string(),
            body: "Looks good.".to_string(),
            attach: Vec::new(),
            to: Some("ale@acme.com, cm@ey.com".to_string()),
            cc: Some(String::new()),
            bcc: None,
            draft_id: None,
        }
        .verb();
        assert_eq!(tool, "post_draft");
        assert_eq!(sending["to"], "ale@acme.com, cm@ey.com");
        assert_eq!(sending["cc"], "");
    }

    /// A job with nothing to do still comes back, so the window never waits
    /// forever on a thread that has already finished.
    #[test]
    fn a_finished_job_comes_back_down_the_channel() {
        let mut jobs = Jobs::new();
        assert!(!jobs.busy());
        // post_accounts reads a file and touches no network.
        jobs.start(7, Job::Accounts, None);
        assert!(jobs.busy());
        let mut answers = Vec::new();
        for _ in 0..200 {
            answers.extend(jobs.drain());
            if !answers.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(answers.len(), 1, "the answer came back");
        assert!(
            matches!(answers[0], (Some(7), Done::Accounts(_))),
            "for the step that asked"
        );
        assert!(!jobs.busy());
    }

    // ---------------------------------------------------- grouping the page

    /// A page of messages as conversations: one line per thread, the newest
    /// message on it is the line, unread if anything on it is unread, and the
    /// count is how many messages it gathered.
    #[test]
    fn a_page_of_messages_is_a_list_of_conversations() {
        let mailbox = Mailbox::of_answer(
            "ed@acme.com",
            &json!({
                "account": "ed@acme.com",
                "messages": [
                    {"id": "18f4", "thread_id": "t-quote", "from": "Ale <ale@acme.com>",
                     "subject": "Re: the quote", "date": "2025-09-12 10:33", "unread": true},
                    {"id": "18f3", "thread_id": "t-quote", "from": "Ed <ed@acme.com>",
                     "subject": "Re: the quote", "date": "2025-09-12 09:10", "unread": false},
                    {"id": "18f2", "thread_id": "t-slides", "from": "EY <cristiano@ey.com>",
                     "subject": "Slides", "date": "2025-09-11 08:02", "unread": false},
                    {"id": "18f1", "thread_id": "t-quote", "from": "Ale <ale@acme.com>",
                     "subject": "The quote", "date": "2025-09-10 19:20", "unread": false}
                ]
            }),
        );
        assert_eq!(mailbox.rows.len(), 4, "the messages are all still here");
        let ids: Vec<&str> = mailbox.threads.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["t-quote", "t-slides"], "newest conversation first");

        let quote = mailbox.thread("t-quote").expect("a conversation");
        assert_eq!(quote.count, 3);
        assert_eq!(quote.latest.id, "18f4", "the newest message is the line");
        assert_eq!(quote.latest.date, "2025-09-12 10:33");
        assert_eq!(quote.subject, "Re: the quote", "and its subject is the line's");
        assert!(quote.unread, "one unread message makes the conversation unread");
        assert_eq!(quote.ids, vec!["18f4", "18f3", "18f1"], "newest first");

        let slides = mailbox.thread("t-slides").expect("a conversation");
        assert_eq!(slides.count, 1);
        assert!(!slides.unread);

        // The account row counts conversations with something new on them, not
        // messages: three of these four rows are one line.
        assert_eq!(mailbox.unread(), 1);
    }

    /// A message the provider filed under no thread is a conversation of one,
    /// addressed by its own id.
    #[test]
    fn a_message_with_no_thread_is_a_conversation_of_its_own() {
        let mailbox = Mailbox::of_answer(
            "ed@acme.com",
            &json!({"messages": [
                {"id": "18f3", "from": "Ale <ale@acme.com>", "subject": "Hi",
                 "date": "2025-09-12 10:33", "unread": true}
            ]}),
        );
        assert_eq!(mailbox.threads.len(), 1);
        assert_eq!(mailbox.threads[0].id, "18f3");
        assert_eq!(mailbox.threads[0].count, 1);
    }

    /// Reading one message of a conversation leaves the conversation read only
    /// once nothing on it is unread, and a trashed message leaves the line —
    /// taking the whole line with it when it was the last one.
    #[test]
    fn a_conversations_weight_follows_its_messages() {
        let mut mailbox = Mailbox::of_answer(
            "ed@acme.com",
            &json!({"messages": [
                {"id": "18f4", "thread_id": "t", "from": "Ale", "subject": "Re: q",
                 "date": "2025-09-12 10:33", "unread": true},
                {"id": "18f3", "thread_id": "t", "from": "Ed", "subject": "q",
                 "date": "2025-09-12 09:10", "unread": true}
            ]}),
        );
        assert_eq!(mailbox.unread(), 1);

        mailbox.set_unread("18f4", false);
        assert!(mailbox.threads[0].unread, "the older one is still unread");

        mailbox.set_unread("18f3", false);
        assert!(!mailbox.threads[0].unread, "and now nothing on it is");

        mailbox.remove("18f4");
        assert_eq!(mailbox.threads.len(), 1, "the conversation is still there");
        assert_eq!(mailbox.threads[0].count, 1);
        assert_eq!(mailbox.threads[0].latest.id, "18f3", "what is left is the line");

        mailbox.remove("18f3");
        assert!(mailbox.threads.is_empty(), "and with nothing on it, it is gone");
    }

    /// A later page carrying an older message of a conversation already on
    /// screen is a higher count on that line, not a second line.
    #[test]
    fn a_later_page_joins_the_conversations_already_listed() {
        let mut mailbox = Mailbox::of_answer(
            "ed@acme.com",
            &json!({"account": "ed@acme.com", "messages": [
                {"id": "18f4", "thread_id": "t-quote", "from": "Ale", "subject": "Re: q",
                 "date": "2025-09-12 10:33", "unread": false}
            ]}),
        );
        assert_eq!(mailbox.threads.len(), 1);
        assert_eq!(mailbox.threads[0].count, 1);

        mailbox.append(Mailbox::of_answer(
            "ed@acme.com",
            &json!({"account": "ed@acme.com", "messages": [
                {"id": "18f1", "thread_id": "t-quote", "from": "Ale", "subject": "q",
                 "date": "2025-09-10 19:20", "unread": true},
                {"id": "18f0", "thread_id": "t-old", "from": "Fredrik", "subject": "Vintervecka",
                 "date": "2025-09-09 08:00", "unread": false}
            ]}),
        ));
        assert_eq!(mailbox.threads.len(), 2, "one line gained a message");
        assert_eq!(mailbox.threads[0].count, 2);
        assert_eq!(mailbox.threads[0].latest.id, "18f4", "the newest is still the line");
        assert!(mailbox.threads[0].unread, "and the older one is unread");
    }

    // --------------------------------------------------- walking one of them

    /// A conversation exactly as `post_thread` answers it.
    fn conversation() -> Conversation {
        Conversation::of_answer(
            "ed@acme.com",
            &json!({
                "account": "ed@acme.com",
                "thread_id": "t-quote",
                "subject": "Re: the quote",
                "count": 3,
                "messages": [
                    {"id": "18f1", "from": "Ale <ale@acme.com>", "date": "2025-09-10 19:20",
                     "subject": "The quote", "unread": false, "text": "Can you send the quote?"},
                    {"id": "18f3", "from": "Ed <ed@acme.com>", "date": "2025-09-12 09:10",
                     "subject": "Re: the quote", "unread": false, "text": "On its way."},
                    {"id": "18f4", "from": "Ale <ale@acme.com>", "date": "2025-09-12 10:33",
                     "subject": "Re: the quote", "unread": true, "text": "Looks good."}
                ]
            }),
        )
    }

    /// A conversation opens at its newest message, open, with the earlier ones
    /// above it — and the sent reply among them.
    #[test]
    fn a_conversation_opens_at_its_newest_message() {
        let thread = conversation();
        assert_eq!(thread.id, "t-quote");
        assert_eq!(thread.subject, "Re: the quote");
        assert_eq!(thread.messages.len(), 3);
        assert_eq!(thread.messages[0].id, "18f1", "oldest first");
        assert_eq!(thread.newest().expect("a message").id, "18f4");
        assert_eq!(thread.focus, 2);
        assert_eq!(thread.open, Some(2));
        assert_eq!(thread.opened().expect("a message").id, "18f4");
        // A reply of this account's own is a message on the conversation like
        // any other.
        assert_eq!(thread.messages[1].from, "Ed <ed@acme.com>");
        assert!(thread.messages[2].unread, "and unread is carried per message");

        // A conversation with nothing on it has nothing open.
        let empty = Conversation::of_answer("ed@acme.com", &json!({"messages": []}));
        assert_eq!(empty.open, None);
        assert_eq!(empty.opened(), None);
        assert_eq!(empty.newest(), None);
    }

    /// Up and Down walk the messages, stopping at either end; Enter opens the
    /// one they are on and closes it again, and only ever one is open.
    #[test]
    fn the_keyboard_walks_the_conversation_and_enter_opens_one() {
        let mut thread = conversation();

        assert!(thread.step(-1));
        assert_eq!(thread.focus, 1);
        assert_eq!(thread.open, Some(2), "walking past one does not open it");
        assert_eq!(thread.focused().expect("a message").id, "18f3");

        thread.toggle();
        assert_eq!(thread.open, Some(1), "and the one that was open closed");
        assert_eq!(thread.opened().expect("a message").id, "18f3");

        thread.toggle();
        assert_eq!(thread.open, None, "the same one again closes it");

        assert!(thread.step(-1));
        assert_eq!(thread.focus, 0);
        assert!(!thread.step(-1), "the oldest is the top");
        assert_eq!(thread.focus, 0);

        assert!(thread.step(9));
        assert_eq!(thread.focus, 2, "and the newest is the bottom");
        assert!(!thread.step(1));

        // A conversation with nothing on it has nothing to walk or to open.
        let mut empty = Conversation::default();
        assert!(!empty.step(1));
        empty.toggle();
        assert_eq!(empty.open, None);
    }

    /// A message trashed out of a conversation leaves it, and the keyboard and
    /// the open message stay on something that is still there.
    #[test]
    fn a_trashed_message_leaves_the_conversation() {
        let mut thread = conversation();
        thread.remove("18f4");
        assert_eq!(thread.messages.len(), 2);
        assert_eq!(thread.focus, 1, "the focus followed the shorter list");
        assert_eq!(thread.opened().expect("a message").id, "18f3");

        // One above the open message shifts it up rather than opening another.
        let mut thread = conversation();
        thread.remove("18f1");
        assert_eq!(thread.open, Some(1));
        assert_eq!(thread.opened().expect("a message").id, "18f4");

        // A message this conversation never had changes nothing.
        thread.remove("nope");
        assert_eq!(thread.messages.len(), 2);

        thread.remove("18f3");
        thread.remove("18f4");
        assert!(thread.messages.is_empty());
        assert_eq!(thread.open, None);
    }
}

/// Moves a file, across filesystems too. The destination is replaced: the
/// save dialog already asked about that.
fn move_file(from: &str, to: &str) -> Result<(), String> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map_err(|e| format!("could not write {to}: {e}"))?;
    let _ = std::fs::remove_file(from);
    Ok(())
}
