//! What the window is holding, and every way that changes.
//!
//! `PostApp` lives here; `app.rs` owns the update loop and command dispatch,
//! and each `ui::*` module adds its own `impl PostApp` block.
//!
//! Nothing in this file draws, and nothing in it touches the network: an answer
//! arrives as a [`Done`] and [`PostApp::absorb`] is the one place it turns into
//! what is on screen. That is why the window's behaviour — a message marked
//! read the moment it is opened, a trashed row leaving the list, Back landing
//! where it should — can be tested with no display and no mailbox.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Receiver;

use eframe::egui::Context;
use omarchy_kit::theme::{Palette, ThemeWatcher};

use crate::client::{Conversation, Done, Draft, Job, Jobs, Label, Mailbox, Message};
use crate::keymap::Keymap;
use crate::layout::Layout;
use crate::route::{INBOX, Route, Router};

pub struct PostApp {
    /// Every step walked through, each with the screen it was left with.
    pub nav: Router<Screen>,
    /// What the step on screen holds. Its own step's copy in `nav` is an empty
    /// stand-in until the reader steps away and it is kept there.
    pub screen: Screen,
    pub keymap: Keymap,

    /// The connected accounts. `None` until they have been read once.
    pub accounts: Option<Vec<String>>,
    /// The name each goes by, where it is not its address.
    pub names: HashMap<String, String>,
    /// True once reading them has been asked for: the sidebar lists them on
    /// every screen, so they are read whatever the window opened at.
    pub accounts_asked: bool,
    /// The columns this frame's width has room for.
    pub layout: Layout,
    /// Each account's folders, with their unread counts, as last read.
    pub folders: HashMap<String, Vec<Label>>,
    /// The accounts whose folders have been asked for since they last changed.
    /// One leaves this when its counts may have moved — a message read, marked
    /// or trashed — and is asked for again when nothing else is in flight.
    pub folders_asked: HashSet<String>,
    /// Set by `/`: the list's search box takes the keyboard next frame.
    pub focus_search: bool,
    /// The column the keyboard is in, as last moved to; [`PostApp::pane`] is
    /// what the screen makes of it.
    pub pane: Pane,
    /// The sidebar's keyboard stop: an account, or a folder of the account on
    /// screen, in the order they are drawn.
    pub side_focus: usize,
    /// Set when the keyboard moved the sidebar's stop, so it scrolls into view.
    pub side_follow: bool,
    /// A swipe-back in the making: horizontal scroll accumulated across
    /// frames, and when it last grew. Cleared on pause or reversal.
    pub swipe: f32,
    pub swipe_at: f64,
    /// Set when the keyboard moved the focus, so the list scrolls the row it
    /// landed on back into view. Cleared by the frame that does it — a mouse
    /// scrolling away from the selection is left alone.
    pub follow_focus: bool,
    /// Points the message body still owes a scroll, from the arrow keys. In
    /// points, positive downwards; taken by the frame that applies it.
    pub body_scroll: f32,

    /// Calls in flight, and their answers.
    pub jobs: Jobs,
    /// True while Google's consent screen is open in the browser.
    pub connecting: bool,
    /// A Google grant died: the status bar offers Reconnect.
    pub reconnect: bool,
    /// The last ask failed: nothing asks again by itself until something
    /// succeeds or the reader acts. What stops a short list from retrying
    /// its next page every frame.
    pub stalled: bool,

    pub status: String,
    /// Every action, found by typing.
    pub command_palette: omarchy_kit::CommandPalette,

    /// Views handed over by a second `post …`.
    pub open_rx: Option<Receiver<Route>>,
    /// A repaint handle, so an answer from a thread lands on screen at once.
    pub wake: Option<Context>,

    pub palette: Palette,
    /// Whether the user lets Claude use Post: `None` until asked.
    pub claude: Option<bool>,
    pub theme_watcher: ThemeWatcher,
    pub last_title: String,

    /// The open message's HTML body.
    pub html: crate::html::Html,
    /// What a file dialog chose. The dialogs run on their own thread: one
    /// open on this one would leave the window deaf to the compositor, which
    /// then calls Post not responding.
    pub picked_tx: std::sync::mpsc::Sender<Picked>,
    pub picked_rx: std::sync::mpsc::Receiver<Picked>,
}

/// What one screen holds. Every step of the history keeps its own, so Back
/// shows a screen as it was left, and an answer lands on the step that asked.
#[derive(Default)]
pub struct Screen {
    /// The page of mail on a list.
    pub mailbox: Option<Mailbox>,
    /// The conversation: every message on it, with one open.
    pub conversation: Option<Conversation>,
    /// The open message's body, ready to draw. `None` when that message is
    /// plain text, when its HTML could not be read back, or when nothing is
    /// open.
    pub body: Option<Page>,
    /// The reply being written: who it goes to and what it is about, as the
    /// mail library addressed it. `None` until the draft comes back.
    pub draft: Option<Draft>,
    /// What has been typed into the reply. The editor owns this until it is
    /// sent — the draft on disk only catches up when it is.
    pub compose: String,
    /// The address fields: their chips, comma separated. Filled once from the
    /// reply-all the mail library worked out, and the reader's after that:
    /// what they say when Send is pressed is what leaves.
    pub to: String,
    pub cc: String,
    pub bcc: String,
    /// What is being typed into To, Cc and Bcc, before it becomes a chip.
    pub typing: [String; 3],
    /// Cc and Bcc are on screen.
    pub copies: bool,
    /// The account it goes from, where the reader picked another than the
    /// one the screen opened for.
    pub from: Option<String>,
    /// The Gmail draft this is saved as, once it has been.
    pub draft_id: Option<String>,
    /// Typed into since it was last saved, and when — the window's clock.
    pub dirty: bool,
    pub edited: f64,
    /// A save is on its way to Gmail.
    pub saving: bool,
    /// Send or Discard, pressed while a save was on its way: done when it lands.
    pub then: Option<Then>,
    /// An account's name as its settings screen is editing it. `None` until
    /// the screen is first drawn, when the name it goes by goes in.
    pub name_edit: Option<String>,
    /// Remove was pressed on an account's settings: the screen asks first.
    pub confirm_remove: bool,
    /// A new message's subject; a reply's comes from what it answers.
    pub subject: String,
    /// The query the fields were last filled from, so they are filled
    /// once and the writer's after that.
    pub filled: Option<String>,
    /// Files chosen to leave with the reply, absolute paths.
    pub attach: Vec<String>,
    /// True between the send key and the answer.
    pub sending: bool,
    /// Which row the keyboard is on, in a list.
    pub focus: usize,
    /// Which stop the keyboard is on, in the account list: one per account,
    /// and "Add account" on the end.
    pub account_focus: usize,
    /// True once this screen's load has been started, so a refusal is said
    /// once rather than retried every frame — and a screen Back returns to
    /// asks for nothing.
    pub asked: bool,
    /// True while the page after the one on screen is on the wire. The list
    /// shows a quiet line at its end and asks for nothing more until it lands.
    pub paging: bool,
    /// Unread, rather than All: the list shows only what is unread in it.
    pub unread: bool,
    /// What the list's search box says. `None` until the list is first drawn,
    /// when a search's own query goes in it.
    pub search: Option<String>,
}

/// The column the keyboard is in. Up and Down walk it; Left and Right move
/// between them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pane {
    Sidebar,
    #[default]
    List,
    Reader,
}

/// What waits for a save to land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Then {
    Send,
    Discard,
}

/// A file dialog's answer.
pub enum Picked {
    /// Files to attach to what is being written.
    Attach(Vec<String>),
    /// Where to save one of a message's attachments.
    SaveAs {
        account: String,
        id: String,
        name: String,
        path: String,
    },
}

impl PostApp {
    /// A window showing `view`, with nothing loaded yet.
    pub fn new(route: Route) -> PostApp {
        let (keymap, warnings) = crate::keymap::load();
        let (picked_tx, picked_rx) = std::sync::mpsc::channel();
        PostApp {
            nav: Router::of(route),
            screen: Screen::default(),
            keymap,
            accounts: None,
            names: HashMap::new(),
            accounts_asked: false,
            layout: Layout::default(),
            folders: HashMap::new(),
            folders_asked: HashSet::new(),
            focus_search: false,
            pane: Pane::default(),
            side_focus: 0,
            side_follow: false,
            swipe: 0.0,
            swipe_at: 0.0,
            follow_focus: false,
            body_scroll: 0.0,
            jobs: Jobs::new(),
            connecting: false,
            reconnect: false,
            stalled: false,
            status: first_warning(&warnings),
            command_palette: omarchy_kit::CommandPalette::default(),
            open_rx: None,
            wake: None,
            palette: omarchy_kit::theme::load(),
            claude: post::claude::choice(),
            theme_watcher: ThemeWatcher::new(),
            last_title: String::new(),
            html: crate::html::Html::default(),
            picked_tx,
            picked_rx,
        }
    }

    /// A new message starts filled from its route's query — once;
    /// after that the fields are the writer's.
    pub fn fill_from_link(&mut self) {
        let Route::New { query, .. } = self.nav.top().clone() else {
            return;
        };
        if self.screen.filled.as_deref() == Some(query.as_str()) {
            return;
        }
        let mail = crate::route::Mailto::parse(&format!("mailto:?{query}"));
        self.screen.filled = Some(query);
        self.screen.copies = !mail.cc.is_empty() || !mail.bcc.is_empty();
        self.screen.to = mail.to;
        self.screen.cc = mail.cc;
        self.screen.bcc = mail.bcc;
        self.screen.subject = mail.subject;
        self.screen.compose = mail.body;
        self.screen.attach = mail.attach;
        self.screen.draft = None;
        self.screen.sending = false;
    }

    /// The name an account goes by: what it was renamed to, or its address.
    pub fn name_of(&self, account: &str) -> String {
        self.names
            .get(account)
            .cloned()
            .unwrap_or_else(|| account.to_string())
    }

    pub fn set_status(&mut self, text: impl Into<String>) {
        self.status = text.into();
    }

    /// The folder the screen is in — where a message opened from here belongs.
    pub fn folder(&self) -> String {
        self.nav.top().folder().unwrap_or(INBOX).to_string()
    }

    /// The account the view is about, if it is about one.
    pub fn account(&self) -> Option<&str> {
        self.nav.top().account()
    }

    /// How many stops the account list has for the keyboard: one per account
    /// connected, and "Add account" on the end. Never zero.
    pub fn account_stops(&self) -> usize {
        self.accounts.as_ref().map_or(0, Vec::len) + 1
    }

    /// The messages loaded, empty when nothing is.
    pub fn rows(&self) -> &[crate::client::Row] {
        self.screen.mailbox.as_ref().map(|m| m.rows.as_slice()).unwrap_or(&[])
    }

    /// The lines on screen — one per conversation. The inbox draws these, the
    /// keyboard walks these, and the rows behind them are what a mark or a
    /// trash amends.
    pub fn threads(&self) -> &[crate::client::Thread] {
        self.screen.mailbox
            .as_ref()
            .map(|m| m.threads.as_slice())
            .unwrap_or(&[])
    }

    /// The message whose body is on screen: the open one of the conversation.
    pub fn message(&self) -> Option<&Message> {
        self.screen.conversation.as_ref()?.opened()
    }

    /// True when the conversation loaded is the one `id` asks for — by the
    /// conversation's own id, or by any message on it, because a window opened
    /// at `post read <email> <id>` knows only a message.
    pub fn showing(&self, id: &str) -> bool {
        self.screen.conversation.as_ref().is_some_and(|thread| {
            thread.id == id || thread.messages.iter().any(|message| message.id == id)
        })
    }

    /// Starts a call for the step on screen, waking the window when it comes
    /// back. Its answer belongs to that step, wherever the reader is by then.
    pub fn start(&mut self, job: Job) {
        self.start_for(self.nav.key(), job);
    }

    /// Starts a call whose answer belongs to the step `key` — the list beside
    /// a message asks for its own pages.
    pub fn start_for(&mut self, key: u64, job: Job) {
        let about = job.about();
        if !about.is_empty() {
            self.set_status(about);
        }
        if job == Job::AddAccount {
            self.connecting = true;
        }
        self.jobs.start(key, job, self.wake.clone());
    }

    /// Steps to a route. The screen being left stays with its step; a new
    /// step starts with nothing loaded, and a step behind comes back as it
    /// was left, with no fetch.
    pub fn goto(&mut self, route: Route) {
        self.keep_writing();
        self.nav.push(route, &mut self.screen);
    }

    /// The screen becomes another without a step for Back to return to.
    pub fn replace(&mut self, route: Route) {
        self.keep_writing();
        self.nav.replace(route, &mut self.screen);
    }

    /// Back one step, to its screen as it was left.
    pub fn back(&mut self) {
        self.keep_writing();
        self.nav.back(&mut self.screen);
    }

    /// Leaving a screen that is being written saves what was typed into
    /// Gmail's Drafts first, so it is there on every device.
    fn keep_writing(&mut self) {
        if self.nav.top().writing() && self.screen.dirty && !self.screen.sending && !self.screen.saving
        {
            self.save_now();
        }
    }

    /// The account what is being written goes from.
    pub fn writing_account(&self) -> Option<String> {
        self.screen
            .from
            .clone()
            .or_else(|| self.account().map(str::to_owned))
    }

    /// What is being written, as the call that writes it into Gmail's Drafts:
    /// a reply through the message it answers, anything else as a new message
    /// — a draft that was a reply keeps its thread that way too.
    pub fn writing_job(&self) -> Option<Job> {
        let s = &self.screen;
        match self.nav.top() {
            Route::Reply { account, id, .. } => Some(Job::Draft {
                account: account.clone(),
                id: id.clone(),
                body: s.compose.clone(),
                attach: s.attach.clone(),
                to: Some(s.to.clone()),
                cc: Some(s.cc.clone()),
                bcc: Some(s.bcc.clone()),
                draft_id: s.draft_id.clone(),
            }),
            Route::Forward { account, id, .. } => Some(Job::Forward {
                account: account.clone(),
                id: id.clone(),
                body: s.compose.clone(),
                subject: Some(s.subject.clone()),
                to: Some(s.to.clone()),
                cc: Some(s.cc.clone()),
                bcc: Some(s.bcc.clone()),
                attach: Some(s.attach.clone()),
                draft_id: s.draft_id.clone(),
            }),
            Route::New { .. } | Route::Draft { .. } => Some(Job::New {
                account: self.writing_account()?,
                to: s.to.clone(),
                cc: s.cc.clone(),
                bcc: s.bcc.clone(),
                subject: s.subject.clone(),
                body: s.compose.clone(),
                attach: s.attach.clone(),
                draft_id: s.draft_id.clone(),
            }),
            _ => None,
        }
    }

    /// Writes what is on screen into Gmail's Drafts now. A reply waits for its
    /// addresses, and a forward for its subject and files: until they land
    /// there is nothing whole to write.
    pub fn save_now(&mut self) {
        if matches!(self.nav.top(), Route::Reply { .. } | Route::Forward { .. })
            && self.screen.draft.is_none()
        {
            return;
        }
        let Some(job) = self.writing_job() else {
            return;
        };
        self.screen.dirty = false;
        self.screen.saving = true;
        self.start(Job::Save(Box::new(job)));
    }

    /// Asks for the page after the one on screen, to go on the end of it.
    ///
    /// Reaching the end of the list is the whole of the ask — the wheel near
    /// the bottom, or the selection landing on the last row. Nothing to do at
    /// the end of a mailbox, or while a page is already on its way.
    pub fn load_more(&mut self) {
        self.load_more_for(self.nav.key());
    }

    /// The same for the list of the step `key`, on screen or beside it.
    pub fn load_more_for(&mut self, key: u64) {
        let Some((_, screen)) = self.step_of(Some(key)) else {
            return;
        };
        if screen.paging {
            return;
        }
        let Some(mailbox) = &screen.mailbox else {
            return;
        };
        let (account, folder, Some(page)) = (
            mailbox.account.clone(),
            mailbox.folder.clone(),
            mailbox.next_page.clone(),
        ) else {
            return;
        };
        screen.paging = true;
        let unread = screen.unread;
        self.start_for(
            key,
            Job::List {
                account,
                folder,
                unread,
                page: Some(page),
            },
        );
    }

    /// Drops the conversation on screen and takes its body off screen, so the
    /// next frame asks for it again.
    pub fn forget_message(&mut self) {
        self.screen.conversation = None;
        self.screen.body = None;
        self.html.close();
        self.screen.asked = false;
    }

    /// Rebuilds the body for whichever message of the conversation on screen
    /// is open. Called whenever that changes — arriving, walking, expanding.
    pub fn show_open_message(&mut self) {
        self.screen.open_body(&self.palette);
        if self.screen.body.is_none() {
            self.html.close();
        }
    }

    /// The route and screen of the step `key` — on screen or behind it — or
    /// nothing once the reader has left that step for good.
    pub fn step_of(&mut self, key: Option<u64>) -> Option<(&Route, &mut Screen)> {
        let key = key?;
        self.nav
            .screens(&mut self.screen)
            .find(|(step, ..)| *step == key)
            .map(|(_, route, screen)| (route, screen))
    }

    /// One answer for the step on screen.
    #[cfg(test)]
    pub fn absorb(&mut self, done: Done) {
        self.answer(Some(self.nav.key()), done);
    }

    /// One answer, as the change it makes. `key` is the step whose call it
    /// was; `None` is a page the mail library refreshed by itself.
    ///
    /// What a call loads lands on the step that asked for it, on screen or
    /// behind, and is dropped once that step is gone. What happened to a
    /// message — read, unread, trashed — reaches every step that shows it.
    ///
    /// Reading a message marks it read — the mail library does that as part of
    /// the read, and the row follows here — so opening a message is the whole
    /// of "mark read on open"; there is no second call to forget.
    pub fn answer(&mut self, key: Option<u64>, done: Done) {
        match done {
            Done::Accounts(list) => {
                self.names = list
                    .iter()
                    .filter(|(email, name)| email != name)
                    .cloned()
                    .collect();
                self.accounts = Some(list.into_iter().map(|(email, _)| email).collect());
            }
            Done::Renamed { account, name } => {
                self.status.clear();
                if name == account {
                    self.names.remove(&account);
                } else {
                    self.names.insert(account, name);
                }
            }
            Done::Removed(account) => {
                if let Some(accounts) = &mut self.accounts {
                    accounts.retain(|a| *a != account);
                }
                self.names.remove(&account);
                self.folders.remove(&account);
                self.folders_asked.remove(&account);
                // Nothing of it is left — on this computer, or in the window:
                // the history starts again from settings.
                self.nav = Router::of(Route::Settings);
                self.screen = Screen::default();
                self.html.close();
                self.set_status(format!("Removed {account} from this computer."));
            }
            Done::Labels { account, labels } => {
                self.folders.insert(account, labels);
            }
            Done::List(mailbox) => {
                self.stalled = false;
                self.status.clear();
                // A page the library refreshed by itself corrects every list
                // of that folder that has a page and shows it all; one still
                // waiting for its own gets that one.
                for (step, _, screen) in self.nav.screens(&mut self.screen) {
                    let ours = match key {
                        Some(key) => step == key,
                        None => {
                            !screen.unread
                                && screen.mailbox.as_ref().is_some_and(|m| {
                                    m.account == mailbox.account && m.folder == mailbox.folder
                                })
                        }
                    };
                    if ours {
                        screen.listed(mailbox.clone());
                    }
                }
            }
            Done::More(more) => {
                self.stalled = false;
                self.status.clear();
                // The page that came back goes on the end of the list that
                // asked for it, and of no other. The selection does not move:
                // rows only ever go on the end.
                if let Some((_, screen)) = self.step_of(key) {
                    screen.paging = false;
                    if let Some(mailbox) = &mut screen.mailbox
                        && mailbox.account == more.account
                    {
                        mailbox.append(more);
                    }
                }
            }
            Done::Thread(conversation) => {
                self.status.clear();
                self.folders_asked.remove(&conversation.account);
                // Showing a conversation reads what was unread on it, so its
                // rows are read in every list — whether or not the reader is
                // still there to see it.
                for (_, _, screen) in self.nav.screens(&mut self.screen) {
                    if let Some(mailbox) = &mut screen.mailbox {
                        for message in &conversation.messages {
                            mailbox.set_unread(&message.id, false);
                        }
                    }
                }
                // The conversation belongs to the step that asked for it. A
                // reader who went back before it landed is not dragged into it.
                if key == Some(self.nav.key()) {
                    self.screen.conversation = Some(conversation);
                    self.show_open_message();
                } else {
                    let palette = self.palette;
                    if let Some((_, screen)) = self.step_of(key) {
                        screen.conversation = Some(conversation);
                        screen.open_body(&palette);
                    }
                }
            }
            Done::Marked { id, read } => {
                self.recount(key);
                for (_, _, screen) in self.nav.screens(&mut self.screen) {
                    screen.marked(&id, read);
                }
                self.set_status(if read {
                    format!("{id} marked read.")
                } else {
                    format!("{id} marked unread.")
                });
            }
            Done::Saved { path, open } => {
                self.set_status(format!("Saved to {path}"));
                if open {
                    open_file(&path);
                }
            }
            Done::Deleted { id } => {
                self.recount(key);
                let (top, palette) = (self.nav.key(), self.palette);
                let mut emptied = false;
                for (step, _, screen) in self.nav.screens(&mut self.screen) {
                    emptied |= screen.deleted(&id, &palette) && step == top;
                }
                // A conversation with nothing left on it is nothing to show.
                if emptied {
                    self.back();
                } else if self.screen.body.is_none() {
                    self.html.close();
                }
                self.set_status(format!("{id} moved to the trash."));
            }
            Done::Drafted(draft) => {
                self.status.clear();
                // The draft a reply opens with is what fills its address
                // fields: the reply-all off the message being answered. After
                // that they are the reader's, and a draft asked for again does
                // not write over them.
                if let Some((route, screen)) = self.step_of(key) {
                    if screen.draft.is_none() {
                        screen.to = draft.to.clone();
                        screen.cc = draft.cc.clone();
                        // A forward opens with its subject and the message's
                        // own files, both the reader's from here.
                        if matches!(route, Route::Forward { .. }) {
                            screen.subject = draft.subject.clone();
                            screen.attach = draft.attachments.clone();
                        }
                    }
                    screen.draft = Some(draft);
                }
            }
            Done::Written(draft) => {
                let on_top = key == Some(self.nav.key());
                let Some((_, screen)) = self.step_of(key) else {
                    return;
                };
                screen.saving = false;
                // A draft Gmail did not have before is one more in Drafts.
                let new = screen.draft_id.is_none() && !draft.id.is_empty();
                if !draft.id.is_empty() {
                    screen.draft_id = Some(draft.id);
                }
                let then = screen.then.take();
                if new {
                    self.recount(key);
                }
                // What was pressed while it was on its way goes now, with the
                // draft's id known, so nothing is written twice.
                match then {
                    Some(Then::Send) if on_top => self.send_now(),
                    Some(Then::Discard) if on_top => self.discard(),
                    _ => {}
                }
            }
            Done::Opened(draft) => {
                self.status.clear();
                if let Some((_, screen)) = self.step_of(key) {
                    screen.copies = !draft.cc.is_empty() || !draft.bcc.is_empty();
                    screen.to = draft.to;
                    screen.cc = draft.cc;
                    screen.bcc = draft.bcc;
                    screen.subject = draft.subject;
                    screen.compose = draft.body;
                    screen.attach = draft.attachments;
                    screen.draft_id = Some(draft.id);
                    screen.dirty = false;
                }
            }
            Done::Discarded { draft_id } => {
                self.recount(key);
                self.forget_draft(&draft_id);
                self.set_status("Draft discarded.");
            }
            Done::Sent { to } => {
                let on_top = key == Some(self.nav.key());
                let (reply, sent) = match self.step_of(key) {
                    Some((route, screen)) => {
                        let sent = screen.draft_id.clone();
                        screen.written();
                        (matches!(route, Route::Reply { .. }), sent)
                    }
                    None => (false, None),
                };
                // A draft that went is no longer in Drafts, and Sent has one
                // more in it.
                self.recount(key);
                if let Some(draft_id) = sent {
                    self.forget_draft(&draft_id);
                }
                if on_top {
                    self.leave_written(true);
                }
                self.set_status(if reply {
                    format!("Reply sent to {to}.")
                } else {
                    format!("Sent to {to}.")
                });
            }
            Done::Added(email) => {
                self.connecting = false;
                self.reconnect = false;
                self.stalled = false;
                let accounts = self.accounts.get_or_insert_with(Vec::new);
                if !accounts.contains(&email) {
                    accounts.push(email.clone());
                }
                // Connected from settings, the account's own mail is what
                // comes next. Fresh grant, fresh mail: the screen on top asks
                // again with the new tokens.
                if self.nav.top() == &Route::Settings {
                    self.replace(Route::inbox(email.clone()));
                }
                self.screen.mailbox = None;
                self.screen.conversation = None;
                self.screen.body = None;
                self.screen.asked = false;
                self.set_status(format!("Connected {email}."));
            }
            Done::Failed { about, error } => {
                self.connecting = false;
                if post::gmail::reconnect_needed(&error) {
                    self.reconnect = true;
                }
                self.stalled = true;
                // The step that asked stops waiting: a page can be asked for
                // again, and a reply that did not go keeps every word of
                // itself for the reader to try again.
                if let Some((_, screen)) = self.step_of(key) {
                    screen.paging = false;
                    screen.sending = false;
                    // What was typed stays in the editor, and the next edit
                    // tries again.
                    screen.saving = false;
                    screen.then = None;
                }
                self.set_status(format!("{about} {error}").trim().to_string());
            }
        }
    }

    /// A draft that was sent or thrown away leaves every list that shows it —
    /// the Drafts folder's, behind the screen it was written on.
    fn forget_draft(&mut self, draft_id: &str) {
        for (_, _, screen) in self.nav.screens(&mut self.screen) {
            if let Some(mailbox) = &mut screen.mailbox {
                let ids: Vec<String> = mailbox
                    .rows
                    .iter()
                    .filter(|row| row.draft_id == draft_id)
                    .map(|row| row.id.clone())
                    .collect();
                for id in ids {
                    mailbox.remove(&id);
                }
            }
        }
    }

    /// The folders of the account the step `key` is about are counted again
    /// when nothing else is in flight: something in them changed.
    fn recount(&mut self, key: Option<u64>) {
        let account = match self.step_of(key) {
            Some((route, _)) => route.account().map(str::to_owned),
            None => self.account().map(str::to_owned),
        };
        if let Some(account) = account {
            self.folders_asked.remove(&account);
        }
    }

    /// Sends what is on screen: written into its Gmail draft and sent from
    /// there, in one job. The checks are the caller's.
    pub fn send_now(&mut self) {
        let Some(job) = self.writing_job() else {
            return;
        };
        self.screen.sending = true;
        self.screen.dirty = false;
        self.start(Job::Deliver(Box::new(job)));
    }

    /// Throws away what is being written: its Gmail draft deleted for good,
    /// and the screen left without saving on the way out. While a save is on
    /// its way it waits for it, so the draft that save writes goes too.
    pub fn discard(&mut self) {
        if !self.nav.top().writing() || self.screen.sending {
            return;
        }
        if self.screen.saving {
            self.screen.then = Some(Then::Discard);
            return;
        }
        if let (Some(account), Some(draft_id)) = (self.writing_account(), self.screen.draft_id.clone()) {
            self.start(Job::DeleteDraft { account, draft_id });
        }
        self.screen.written();
        self.leave_written(false);
    }

    /// Where the screen on top goes once what it was writing has left, sent
    /// or thrown away. A reply goes back to its conversation — asked for again
    /// when a reply went, so it is on it; a new message goes back where the
    /// writer came from, or — in a window opened straight at it — to that
    /// account's mail.
    fn leave_written(&mut self, sent: bool) {
        match self.nav.top().clone() {
            // Its conversation is always the step behind it.
            Route::Reply { .. } | Route::Forward { .. } => {
                self.back();
                if sent {
                    self.forget_message();
                }
            }
            Route::New { account, .. } | Route::Draft { account, .. } => {
                if self.nav.depth() > 1 {
                    self.back();
                } else {
                    self.replace(Route::inbox(account));
                }
            }
            _ => {}
        }
    }
}

impl Screen {
    /// A page of the list arrived. The same mailbox again — a cached page
    /// corrected — keeps the selection where the reader put it.
    fn listed(&mut self, mailbox: Mailbox) {
        let same = self
            .mailbox
            .as_ref()
            .is_some_and(|m| m.account == mailbox.account);
        self.focus = if same {
            self.focus.min(mailbox.threads.len().saturating_sub(1))
        } else {
            0
        };
        self.mailbox = Some(mailbox);
    }

    /// Builds the body for whichever message of the conversation is open.
    fn open_body(&mut self, palette: &Palette) {
        self.body = self
            .conversation
            .as_ref()
            .and_then(Conversation::opened)
            .and_then(|message| body_page(message, palette));
    }

    /// A message was marked read or unread: its row, and its line in the
    /// conversation, follow.
    fn marked(&mut self, id: &str, read: bool) {
        if let Some(mailbox) = &mut self.mailbox {
            mailbox.set_unread(id, !read);
        }
        if let Some(conversation) = &mut self.conversation
            && let Some(message) = conversation.messages.iter_mut().find(|m| m.id == id)
        {
            message.unread = !read;
        }
    }

    /// A message went to the trash: its row leaves the list and it leaves the
    /// conversation. True when that left the conversation with nothing on it.
    fn deleted(&mut self, id: &str, palette: &Palette) -> bool {
        if let Some(mailbox) = &mut self.mailbox {
            mailbox.remove(id);
            self.focus = self.focus.min(mailbox.threads.len().saturating_sub(1));
        }
        let Some(conversation) = &mut self.conversation else {
            return false;
        };
        if !conversation.messages.iter().any(|m| m.id == id) {
            return false;
        }
        conversation.remove(id);
        if conversation.messages.is_empty() {
            return true;
        }
        self.open_body(palette);
        false
    }

    /// What was being written has left, sent or thrown away: the editor is
    /// empty again, and there is nothing left to save on the way out.
    fn written(&mut self) {
        self.sending = false;
        self.compose.clear();
        self.to.clear();
        self.cc.clear();
        self.bcc.clear();
        self.typing = Default::default();
        self.subject.clear();
        self.attach.clear();
        self.draft = None;
        self.draft_id = None;
        self.dirty = false;
        self.saving = false;
        self.then = None;
    }
}

/// What a keymap complained about, in one line. A file written for another app
/// — `./keymap.toml` in a directory that is not Post's — objects to every
/// binding in it, and a status bar is not the place to read all of them.
fn first_warning(warnings: &[String]) -> String {
    match warnings.split_first() {
        None => String::new(),
        Some((first, [])) => first.clone(),
        Some((first, rest)) => format!("{first} (and {} more)", rest.len()),
    }
}

/// One message's body, as the detail view draws it.
pub struct Page {
    /// The message's saved HTML, in the page `html::page` writes around it.
    pub html: String,
}

/// The open message's saved HTML as a page to render — or nothing when there
/// is no HTML to read, in which case the detail view shows the text body.
fn body_page(message: &Message, palette: &Palette) -> Option<Page> {
    let path = message.html_path.as_deref()?;
    let html = std::fs::read_to_string(path).ok()?;
    let rgb = |c: eframe::egui::Color32| [c.r(), c.g(), c.b()];
    Some(Page {
        html: crate::html::page(&html, rgb(palette.accent)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{Mailbox, Message};
    use serde_json::json;

    fn listing() -> Mailbox {
        Mailbox::of_answer(
            "ed@acme.com",
            &json!({
                "account": "ed@acme.com",
                "messages": [
                    {"id": "18f3", "thread_id": "18f3", "from": "Ale <ale@acme.com>",
                     "subject": "Re: the quote", "date": "2025-09-12 10:33", "unread": true},
                    {"id": "18f2", "thread_id": "18f2", "from": "EY <cristiano@ey.com>",
                     "subject": "Slides", "date": "2025-09-11 08:02", "unread": true}
                ]
            }),
        )
    }

    fn message(id: &str) -> Message {
        Message {
            id: id.to_string(),
            account: "ed@acme.com".to_string(),
            from: "Ale <ale@acme.com>".to_string(),
            subject: "Re: the quote".to_string(),
            date: "2025-09-12 10:33".to_string(),
            text: "Looks good — send it.".to_string(),
            html_path: None,
            attachments: Vec::new(),
            unread: false,
        }
    }

    /// A conversation of one message, exactly as `post_thread` answers for a
    /// message nobody has replied to.
    fn thread(id: &str) -> crate::client::Conversation {
        crate::client::Conversation {
            account: "ed@acme.com".to_string(),
            id: id.to_string(),
            subject: "Re: the quote".to_string(),
            messages: vec![message(id)],
            focus: 0,
            open: Some(0),
        }
    }

    /// A page of `ids`, and where the mailbox continues after it.
    fn paged(ids: &[&str], next: Option<&str>) -> Mailbox {
        let messages: Vec<_> = ids
            .iter()
            .map(|id| {
                json!({"id": id, "from": "Ale <ale@acme.com>", "subject": "Re: the quote",
                       "date": "2025-09-12 10:33", "unread": true})
            })
            .collect();
        Mailbox::of_answer(
            "ed@acme.com",
            &json!({"account": "ed@acme.com", "messages": messages, "next_page": next}),
        )
    }

    fn inbox() -> PostApp {
        let mut app = PostApp::new(Route::inbox("ed@acme.com".to_string()));
        app.absorb(Done::List(listing()));
        app
    }

    /// A row tapped, and the answer back: the whole of opening a
    /// conversation.
    fn opened(id: &str) -> PostApp {
        let mut app = inbox();
        app.open("ed@acme.com", id);
        app.absorb(Done::Thread(thread(id)));
        app
    }

    /// Opening a message reads it, and a read message is no longer bold.
    #[test]
    fn opening_a_message_marks_its_row_read() {
        let mut app = inbox();
        assert!(app.screen.mailbox.as_ref().expect("a page").row("18f3").expect("a row").unread);
        assert_eq!(app.screen.mailbox.as_ref().expect("a page").unread(), 2);

        app.open("ed@acme.com", "18f3");
        app.absorb(Done::Thread(thread("18f3")));

        // The message is on screen, with the inbox behind it.
        assert_eq!(
            app.nav.top(),
            &Route::Message { account: "ed@acme.com".to_string(), folder: INBOX.to_string(), id: "18f3".to_string() }
        );
        assert_eq!(app.nav.depth(), 2, "the window opened at the inbox");
        assert_eq!(app.message().expect("a message").id, "18f3");

        app.back();
        let mailbox = app.screen.mailbox.as_ref().expect("a page");
        assert!(!mailbox.row("18f3").expect("a row").unread, "the row is read now");
        assert!(mailbox.row("18f2").expect("a row").unread, "and only that row");
        assert_eq!(mailbox.unread(), 1);
    }

    /// A reader who goes back before the message arrives stays where they are —
    /// and the message is still read, because reading it is what marked it.
    #[test]
    fn an_answer_that_arrives_too_late_does_not_drag_anyone_back() {
        let mut app = inbox();
        app.open("ed@acme.com", "18f3");
        let asked = app.nav.key();
        app.back();
        app.answer(Some(asked), Done::Thread(thread("18f3")));

        assert_eq!(
            app.nav.top(),
            &Route::inbox("ed@acme.com".to_string())
        );
        assert!(app.message().is_none(), "nothing was forced open");
        assert!(
            !app.screen.mailbox.as_ref().expect("a page").row("18f3").expect("a row").unread,
            "it was read all the same"
        );
    }

    /// Marking it unread again puts the weight back.
    #[test]
    fn a_message_can_be_made_unread_again() {
        let mut app = opened("18f3");
        app.absorb(Done::Marked {
            id: "18f3".to_string(),
            read: false,
        });
        assert!(app.message().expect("a message").unread, "its line in the conversation");
        assert!(app.status.contains("marked unread"), "got {}", app.status);
        // And the row in the list behind it, which was not on screen.
        app.back();
        assert!(app.screen.mailbox.as_ref().expect("a page").row("18f3").expect("a row").unread);

        app.absorb(Done::Marked {
            id: "18f3".to_string(),
            read: true,
        });
        assert!(!app.screen.mailbox.as_ref().expect("a page").row("18f3").expect("a row").unread);
    }

    /// Trashing the message on screen drops its row and goes back to the list —
    /// never leaving a detail view of something that is gone.
    #[test]
    fn trashing_the_open_message_falls_back_to_the_list() {
        let mut app = opened("18f3");
        app.absorb(Done::Deleted {
            id: "18f3".to_string(),
        });

        assert_eq!(app.rows().len(), 1);
        assert!(app.screen.mailbox.as_ref().expect("a page").row("18f3").is_none());
        assert_eq!(
            app.nav.top(),
            &Route::inbox("ed@acme.com".to_string())
        );
        assert!(app.message().is_none(), "nothing is left open");
        assert!(app.screen.body.is_none());
        assert!(app.status.contains("trash"), "got {}", app.status);
    }

    /// Trashing a row from the list leaves the list where it is.
    #[test]
    fn trashing_a_row_from_the_list_stays_in_the_list() {
        let mut app = inbox();
        app.screen.focus = 1;
        app.absorb(Done::Deleted {
            id: "18f2".to_string(),
        });
        assert_eq!(app.rows().len(), 1);
        assert_eq!(
            app.nav.top(),
            &Route::inbox("ed@acme.com".to_string())
        );
        assert_eq!(app.screen.focus, 0, "the focus follows the shorter list");
    }

    /// Back from a message drops it and keeps the list; back from the view the
    /// window opened at does nothing, because nothing was walked through.
    #[test]
    fn back_drops_what_only_that_view_was_showing() {
        let mut app = opened("18f3");
        app.back();
        assert!(app.message().is_none());
        assert!(app.screen.mailbox.is_some(), "the list is still loaded");
        let inbox = Route::inbox("ed@acme.com".to_string());
        assert_eq!(app.nav.top(), &inbox);
        app.back();
        assert_eq!(app.nav.top(), &inbox, "the inbox it opened at is the floor");
    }

    /// Another account is another mailbox: the rows on screen do not follow it.
    #[test]
    fn changing_account_forgets_the_page_that_was_open() {
        let mut app = inbox();
        app.screen.focus = 1;
        app.goto(Route::inbox("ale@acme.com".to_string()));
        assert!(app.screen.mailbox.is_none());
        assert_eq!(app.screen.focus, 0);
        assert_eq!(app.account(), Some("ale@acme.com"));
    }

    /// A connected account joins the list without a second read of the file,
    /// and the browser wait is over.
    #[test]
    fn a_connected_account_joins_the_list() {
        let mut app = PostApp::new(Route::Settings);
        app.absorb(Done::Accounts(vec![("ed@acme.com".to_string(), "ed@acme.com".to_string())]));
        app.connecting = true;
        app.absorb(Done::Added("ale@acme.com".to_string()));
        assert!(!app.connecting);
        assert_eq!(
            app.accounts.as_deref(),
            Some(["ed@acme.com".to_string(), "ale@acme.com".to_string()].as_slice())
        );
        // Connected from settings, its mail is where the window goes — in
        // place of settings, so Back does not return there.
        assert_eq!(app.nav.top(), &Route::inbox("ale@acme.com"));
        assert_eq!(app.nav.depth(), 1);
        // Connecting one that is already there does not double it up.
        app.absorb(Done::Added("ale@acme.com".to_string()));
        assert_eq!(app.accounts.as_ref().expect("a list").len(), 2);
    }

    /// A call that failed says so and stops waiting — it never leaves the
    /// window pretending something is still in flight.
    #[test]
    fn a_refusal_is_said_out_loud() {
        let mut app = PostApp::new(Route::Settings);
        app.connecting = true;
        app.absorb(Done::Failed {
            about: "Fetching ed@acme.com…".to_string(),
            error: "Gmail refused the call (403)".to_string(),
        });
        assert!(!app.connecting);
        assert!(app.status.contains("403"), "got {}", app.status);
        assert!(app.status.contains("Fetching"), "got {}", app.status);
    }

    /// A keymap written for another app objects to every line in it; the
    /// status bar says the first and how many followed, never all of them.
    #[test]
    fn a_pile_of_keymap_warnings_is_one_line() {
        assert_eq!(first_warning(&[]), "");
        assert_eq!(
            first_warning(&["keymap: unknown command \"move_up\"".to_string()]),
            "keymap: unknown command \"move_up\""
        );
        let many: Vec<String> = (0..40).map(|i| format!("warning {i}")).collect();
        let line = first_warning(&many);
        assert_eq!(line, "warning 0 (and 39 more)");
        assert!(!line.contains("warning 7"));
    }

    /// A window opened straight at a message — `post read ed@… 18f3` —
    /// has the message's folder behind it: Back goes up one level, to the list,
    /// which loads when it is first shown.
    #[test]
    fn a_window_opened_at_a_message_has_its_folder_behind_it() {
        let detail = Route::Message { account: "ed@acme.com".to_string(), folder: INBOX.to_string(), id: "18f3".to_string() };
        let mut app = PostApp::new(detail.clone());
        assert_eq!(app.nav.depth(), 2);
        app.absorb(Done::Thread(thread("18f3")));
        assert_eq!(app.message().expect("a message").id, "18f3");

        app.back();
        let inbox = Route::inbox("ed@acme.com".to_string());
        assert_eq!(app.nav.top(), &inbox);
        assert!(app.screen.mailbox.is_none(), "nothing loaded yet");
        app.back();
        assert_eq!(app.nav.top(), &inbox, "and the folder is the floor");
    }

    // --------------------------------------------------------------- paging

    /// The page after the one on screen is asked for once: while it is on the
    /// wire the list asks for nothing more, and at the end of a mailbox — or
    /// with no mailbox at all — there is nothing to ask for.
    #[test]
    fn a_page_is_asked_for_once_and_only_where_there_is_one() {
        let mut app = inbox();
        app.absorb(Done::List(paged(&["18f3", "18f2"], Some("tok3n"))));

        app.load_more();
        assert!(app.screen.paging, "a page is on its way");
        assert_eq!(
            app.status,
            Job::List {
                account: "ed@acme.com".to_string(),
                folder: INBOX.to_string(),
                unread: false,
                page: Some("tok3n".to_string()),
            }
            .about()
        );

        // A second ask while that one is in flight starts nothing: the status
        // a started job sets is not set again.
        app.set_status("");
        app.load_more();
        assert_eq!(app.status, "", "nothing was started");
        assert!(app.screen.paging);
    }

    /// The end of a mailbox has no token, so there is no page to ask for — and
    /// asking leaves the list where it is rather than waiting forever.
    #[test]
    fn there_is_nothing_to_page_at_the_end_of_a_mailbox() {
        let mut app = inbox();
        assert_eq!(app.screen.mailbox.as_ref().expect("a page").next_page, None);
        app.set_status("");
        app.load_more();
        assert!(!app.screen.paging);
        assert_eq!(app.status, "");

        // And a window with nothing loaded yet has nothing to page either.
        let mut app = PostApp::new(Route::inbox("ed@acme.com".to_string()));
        app.set_status("");
        app.load_more();
        assert!(!app.screen.paging);
        assert_eq!(app.status, "");
    }

    /// The page that comes back goes on the end, and the reader's place in the
    /// list does not move: rows only ever arrive below them. A row that is
    /// somehow in both pages — the boundary shifted under a mailbox that took
    /// a new message meanwhile — is in the list once.
    #[test]
    fn the_page_after_goes_on_the_end_and_the_selection_stays() {
        let mut app = inbox();
        app.absorb(Done::List(paged(&["18f3", "18f2"], Some("tok3n"))));
        app.screen.focus = 1;
        app.screen.paging = true;
        app.set_status("Fetching more…");

        app.absorb(Done::More(paged(&["18f2", "18f1", "18f0"], None)));

        let ids: Vec<&str> = app.rows().iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, vec!["18f3", "18f2", "18f1", "18f0"], "and nothing twice");
        assert_eq!(app.screen.focus, 1, "the reader stayed where they were");
        assert!(!app.follow_focus, "so there is nothing to scroll to");
        assert!(!app.screen.paging, "and the list may ask again");
        assert_eq!(app.status, "");
        assert_eq!(
            app.screen.mailbox.as_ref().expect("a page").next_page,
            None,
            "that was the last page"
        );
    }

    /// A page belongs to the mailbox that asked for it, and to no other: a
    /// reader who changed account while it was on the wire is left alone.
    #[test]
    fn a_page_for_another_mailbox_is_not_appended() {
        let mut app = inbox();
        app.screen.paging = true;
        let mut elsewhere = paged(&["aaa", "bbb"], None);
        elsewhere.account = "ale@acme.com".to_string();

        app.absorb(Done::More(elsewhere));
        assert_eq!(app.rows().len(), 2, "this mailbox is as it was");
        assert!(app.screen.mailbox.as_ref().expect("a page").row("aaa").is_none());
        assert!(!app.screen.paging, "and the list is not left waiting");
    }

    /// A page that failed takes the line off the end of the list — the reader
    /// can ask again, rather than watching it wait forever.
    #[test]
    fn a_page_that_failed_stops_the_paging() {
        let mut app = inbox();
        app.screen.paging = true;
        app.absorb(Done::Failed {
            about: "Fetching more…".to_string(),
            error: "Gmail refused the call (429)".to_string(),
        });
        assert!(!app.screen.paging);
        assert!(app.status.contains("429"), "got {}", app.status);
    }

    /// With no accounts the window still has something to say.
    #[test]
    fn an_empty_account_list_is_a_state_of_its_own() {
        let mut app = PostApp::new(Route::Settings);
        assert_eq!(app.accounts, None, "nothing read yet");
        app.absorb(Done::Accounts(Vec::new()));
        assert_eq!(app.accounts.as_deref(), Some([].as_slice()));
        assert!(app.rows().is_empty());
    }

    // ---------------------------------------------------- the conversation

    /// Showing a conversation reads every unread message on it, and the lines
    /// behind it follow — including the ones that are not the one tapped.
    #[test]
    fn opening_a_conversation_reads_every_message_on_it() {
        let mut app = PostApp::new(Route::inbox("ed@acme.com".to_string()));
        app.absorb(Done::List(Mailbox::of_answer(
            "ed@acme.com",
            &json!({"account": "ed@acme.com", "messages": [
                {"id": "18f4", "thread_id": "t", "from": "Ale", "subject": "Re: q",
                 "date": "2025-09-12 10:33", "unread": true},
                {"id": "18f3", "thread_id": "t", "from": "Ed", "subject": "q",
                 "date": "2025-09-12 09:10", "unread": true},
                {"id": "18f2", "thread_id": "other", "from": "EY", "subject": "Slides",
                 "date": "2025-09-11 08:02", "unread": true}
            ]}),
        )));
        assert_eq!(app.threads().len(), 2, "three messages, two lines");
        assert_eq!(app.screen.mailbox.as_ref().expect("a page").unread(), 2);

        app.open("ed@acme.com", "t");
        app.absorb(Done::Thread(crate::client::Conversation::of_answer(
            "ed@acme.com",
            &json!({"thread_id": "t", "subject": "Re: q", "messages": [
                {"id": "18f3", "from": "Ed", "date": "2025-09-12 09:10", "text": "q"},
                {"id": "18f4", "from": "Ale", "date": "2025-09-12 10:33", "text": "Re: q"}
            ]}),
        )));

        // The newest message is the one open, at the bottom of the list.
        assert_eq!(app.message().expect("a message").id, "18f4");

        app.back();
        let mailbox = app.screen.mailbox.as_ref().expect("a page");
        assert!(!mailbox.row("18f3").expect("a row").unread, "both were read");
        assert!(!mailbox.row("18f4").expect("a row").unread);
        assert!(mailbox.row("18f2").expect("a row").unread, "and only that one");
        assert_eq!(mailbox.unread(), 1);
    }

    /// A window opened at a message id — `post read ed@… 18f4` — gets
    /// the conversation that message is on, and does not ask for it again.
    #[test]
    fn a_conversation_answers_for_any_message_on_it() {
        let mut app = PostApp::new(Route::Message { account: "ed@acme.com".to_string(), folder: INBOX.to_string(), id: "18f4".to_string() });
        assert!(!app.showing("18f4"), "nothing loaded yet");
        app.absorb(Done::Thread(crate::client::Conversation::of_answer(
            "ed@acme.com",
            &json!({"thread_id": "t", "subject": "Re: q", "messages": [
                {"id": "18f3", "from": "Ed", "date": "2025-09-12 09:10", "text": "q"},
                {"id": "18f4", "from": "Ale", "date": "2025-09-12 10:33", "text": "Re: q"}
            ]}),
        )));
        assert!(app.showing("18f4"), "the message asked for is on it");
        assert!(app.showing("t"), "and so is the conversation's own id");
        assert!(!app.showing("nope"));
    }

    /// A reply that went is a message on the conversation now: the reader lands
    /// back on it and it is asked for again, so the reply is there to read.
    #[test]
    fn a_sent_reply_leaves_the_conversation_to_be_fetched_again() {
        let mut app = opened("18f3");
        app.goto(Route::Reply { account: "ed@acme.com".to_string(), folder: INBOX.to_string(), id: "18f3".to_string() });
        app.screen.sending = true;
        app.absorb(Done::Sent {
            to: "ale@acme.com".to_string(),
        });

        assert_eq!(
            app.nav.top(),
            &Route::Message { account: "ed@acme.com".to_string(), folder: INBOX.to_string(), id: "18f3".to_string() }
        );
        assert!(app.screen.conversation.is_none(), "so the next frame asks for it again");
        assert!(!app.screen.asked);
        assert!(app.status.contains("ale@acme.com"), "got {}", app.status);
    }
}

/// Hands a file to whatever opens that kind of file.
pub fn open_file(path: &str) {
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(target_os = "macos"))]
    let opener = "xdg-open";
    let _ = std::process::Command::new(opener)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
