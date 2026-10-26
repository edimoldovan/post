//! Update loop, input handling and command dispatch.
//!
//! Drawing lives in the `ui` modules, each of which adds methods to `PostApp`
//! (defined in `state.rs`). What is here is the frame: step the webview's own
//! loop, take whatever a second command line handed over, absorb the answers
//! that came back from the network, gather commands from the keymap, the
//! sidebar and the screens, run them, then draw.

use eframe::egui::{Context, ViewportCommand};

use crate::client::Job;
use crate::commands::Command;
use crate::route::Route;
use crate::state::{Pane, PostApp};

/// How far one arrow key moves a message body, in points. About three lines —
/// far enough to be reading, near enough to keep your place.
const BODY_STEP: f32 = 60.0;

/// How far PageDown and PageUp go: ten rows of a list, and the same ten steps
/// of a message body — a screenful either way.
const PAGE_STEP: i32 = 10;

/// How long typing has to pause before what was typed is saved to Gmail's
/// Drafts, in seconds.
const SAVE_AFTER: f64 = 2.0;

impl eframe::App for PostApp {
    fn update(&mut self, ctx: &Context, frame: &mut eframe::Frame) {
        if self.wake.is_none() {
            self.wake = Some(ctx.clone());
        }
        // Restyle in place when the Omarchy theme changes under us. The body
        // takes the new colours when it is next built — the next message
        // opened.
        if let Some(palette) = self.theme_watcher.changed(ctx) {
            self.palette = palette;
            ctx.set_visuals(omarchy_kit::theme::visuals(&palette));
        }
        self.take_handoffs(ctx);
        self.fill_from_link();
        for (key, done) in self.jobs.drain() {
            self.answer(key, done);
        }
        while let Ok(picked) = self.picked_rx.try_recv() {
            match picked {
                crate::state::Picked::Attach(files) => {
                    for file in files {
                        if !self.screen.attach.contains(&file) {
                            self.screen.attach.push(file);
                        }
                    }
                }
                crate::state::Picked::SaveAs { account, id, name, path } => {
                    self.start(Job::Attachment { account, id, name, path: Some(path), open: false });
                }
            }
        }
        // Whatever view we ended up on, a body belongs only to a message.
        if self.nav.top().message().is_none() {
            self.html.close();
        }

        // The width decides the columns, every frame: resizing shows the same
        // screens with more or fewer of them.
        self.layout = crate::layout::Layout::of(ctx.content_rect().width());
        let mut cmds = self.collect_input(ctx);
        cmds.extend(self.sidebar(ctx));
        self.status_bar(ctx);
        cmds.extend(self.view(ctx, frame));

        for cmd in cmds {
            self.exec(cmd, ctx);
        }

        self.palette_view(ctx);
        self.claude_question(ctx);
        self.update_title(ctx);
        self.autosave(ctx);
        self.ensure_loaded();
        // A call in flight repaints when it lands; keep the window ticking so
        // the pending line does not sit there after the answer arrives.
        if self.jobs.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }
}

/// Whether a chord types nothing into a text field: one with Ctrl, Alt or
/// Command on it, or a function key.
fn types_nothing(shortcut: &eframe::egui::KeyboardShortcut) -> bool {
    let m = shortcut.modifiers;
    let name = shortcut.logical_key.name();
    m.ctrl
        || m.alt
        || m.command
        || (name.len() > 1 && name.starts_with('F') && name[1..].chars().all(|c| c.is_ascii_digit()))
}

impl PostApp {
    /// Views handed over by a second `post …`: go there, and come to
    /// the front, because someone typed a command and is waiting to see it.
    fn take_handoffs(&mut self, ctx: &Context) {
        let mut arrived = Vec::new();
        if let Some(rx) = &self.open_rx {
            while let Ok(view) = rx.try_recv() {
                arrived.push(view);
            }
        }
        let Some(view) = arrived.pop() else {
            return;
        };
        self.goto(view);
        // The keyboard goes where the command sent the window: a list, or what
        // is open.
        self.pane = match self.nav.top() {
            Route::Folder { .. } | Route::Settings => Pane::List,
            _ => Pane::Reader,
        };
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        hyprland_front();
    }

    /// The commands the keyboard asked for this frame.
    fn collect_input(&mut self, ctx: &Context) -> Vec<Command> {
        let mut cmds = Vec::new();
        // The palette opens even while a text field has the keyboard — by
        // the chords that type nothing, so no binding can eat a letter.
        ctx.input_mut(|input| {
            for (shortcut, cmd) in &self.keymap.bindings {
                if *cmd == Command::Palette && types_nothing(shortcut) && input.consume_shortcut(shortcut) {
                    cmds.push(Command::Palette);
                }
            }
        });
        // While a text field or the palette has the keyboard, the window's
        // own chords stay out of the way.
        if self.command_palette.is_open() || ctx.wants_keyboard_input() {
            return cmds;
        }
        ctx.input_mut(|input| {
            for (shortcut, cmd) in &self.keymap.bindings {
                if input.consume_shortcut(shortcut) {
                    cmds.push(*cmd);
                }
            }
            // The mouse's own back/forward buttons.
            if input.pointer.button_pressed(eframe::egui::PointerButton::Extra1) {
                cmds.push(Command::Back);
            }
            if input.pointer.button_pressed(eframe::egui::PointerButton::Extra2) {
                cmds.push(Command::Open);
            }
            // Two-finger swipe right = back, the way browsers read it: the
            // horizontal scroll a view with nothing to scroll sideways
            // receives, accumulated until it is unmistakably a swipe.
            let d = input.raw_scroll_delta;
            if d.x.abs() > d.y.abs() && d.x != 0.0 {
                let now = input.time;
                if now - self.swipe_at > 0.4 || self.swipe * d.x < 0.0 {
                    self.swipe = 0.0;
                }
                self.swipe += d.x;
                self.swipe_at = now;
                if self.swipe > 120.0 {
                    self.swipe = 0.0;
                    cmds.push(Command::Back);
                }
            }
        });
        cmds
    }

    /// Runs one command.
    pub fn exec(&mut self, cmd: Command, ctx: &Context) {
        match cmd {
            Command::Back => self.back(),
            Command::Accounts => {
                self.goto(Route::Settings);
                self.pane = Pane::List;
            }
            Command::Refresh => self.refresh(),
            Command::Open => self.open_focused(),
            Command::Next => self.step(1),
            Command::Previous => self.step(-1),
            Command::PageNext => self.page(PAGE_STEP),
            Command::PagePrevious => self.page(-PAGE_STEP),
            Command::Last => self.to_end(),
            Command::Delete => self.delete_current(),
            Command::MarkUnread => self.mark_unread(),
            Command::Reply => self.reply(false),
            Command::ReplyAll => self.reply(true),
            Command::Forward => self.forward(),
            Command::Left => self.left(),
            Command::Right => self.right(),
            Command::New => self.new_message(),
            Command::Send => self.send_reply(),
            Command::AddAccount => {
                if !self.connecting {
                    self.start(Job::AddAccount);
                }
            }
            Command::Copy => {
                if let Some(text) = self.html.selected_text() {
                    ctx.copy_text(text);
                }
            }
            Command::Palette => self.command_palette.toggle(),
            Command::Attach => {
                if self.nav.top().writing() && !self.screen.sending {
                    crate::ui::compose::pick_attachments(self.picked_tx.clone(), ctx.clone());
                }
            }
            Command::Copies => {
                if self.nav.top().writing() {
                    self.screen.copies = true;
                }
            }
            Command::AllMail => self.show_unread(false),
            Command::UnreadOnly => self.show_unread(true),
            Command::Search => self.focus_search = true,
            Command::Discard => self.discard(),
            Command::Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
        }
    }

    /// All or Unread on the list on screen, as its toggle does.
    fn show_unread(&mut self, unread: bool) {
        let Some(step) = self.list_step() else {
            return;
        };
        let Some((route, screen)) = self.step_of(Some(step)) else {
            return;
        };
        let (Some(account), Some(folder)) = (route.account(), route.folder()) else {
            return;
        };
        if screen.unread != unread {
            let (account, folder) = (account.to_string(), folder.to_string());
            self.filter(step, &account, &folder, unread);
        }
    }

    /// Asks the provider again for whatever is on screen — and only that: the
    /// steps behind keep what they have.
    fn refresh(&mut self) {
        match self.nav.top().clone() {
            Route::Settings => {
                self.accounts = None;
                self.accounts_asked = false;
            }
            // What is on screen stays on screen: the fresh page replaces it
            // when it lands, so a refresh is one visible change, not a blank
            // and two reloads.
            Route::Folder { account, folder } => {
                self.screen.paging = false;
                self.folders_asked.remove(&account);
                self.start(Job::List {
                    account,
                    folder,
                    unread: self.screen.unread,
                    page: None,
                });
            }
            Route::Message { account, id, .. } => {
                self.start(Job::Thread { account, id });
            }
            // The headers again, off the message being answered. What has been
            // typed is the reader's and is not refreshed away.
            Route::Reply { .. } => {
                self.screen.draft = None;
                self.screen.asked = false;
            }
            // What is being written is the reader's, not the provider's; an
            // account's settings are this computer's.
            Route::New { .. } | Route::Draft { .. } | Route::Forward { .. } | Route::Account { .. } => {}
        }
    }

    /// Writes a reply to the conversation on screen: the editor opens empty,
    /// with the address and the subject on their way.
    ///
    /// A reply answers the newest message on the conversation, whichever one
    /// the keyboard happens to be resting on — that is what "reply" means in a
    /// conversation, and it is what keeps it on one thread.
    fn reply(&mut self, all: bool) {
        if self.nav.top().message().is_none() {
            return;
        }
        let (Some(account), Some(id)) = (
            self.account().map(str::to_owned),
            self.screen.conversation
                .as_ref()
                .and_then(|thread| thread.newest())
                .map(|message| message.id.clone()),
        ) else {
            return;
        };
        self.goto(Route::Reply { account, folder: self.folder(), id, all });
    }

    /// Forwards the message open in the conversation on screen — the one being
    /// read — or, with none open, the newest on it.
    fn forward(&mut self) {
        if self.nav.top().message().is_none() {
            return;
        }
        let (Some(account), Some(id)) = (
            self.account().map(str::to_owned),
            self.screen.conversation.as_ref().and_then(|thread| {
                thread
                    .opened()
                    .or_else(|| thread.newest())
                    .map(|message| message.id.clone())
            }),
        ) else {
            return;
        };
        self.goto(Route::Forward {
            account,
            folder: self.folder(),
            id,
        });
    }

    /// A new email, every field empty, from the account on screen — or, on the
    /// account list, the selected one. Not while something is being written:
    /// that would throw it away.
    fn new_message(&mut self) {
        if self.nav.top().writing() {
            return;
        }
        let account = self.account().map(str::to_owned).or_else(|| {
            self.accounts
                .as_ref()
                .and_then(|list| list.get(self.screen.account_focus))
                .cloned()
        });
        let Some(account) = account else {
            self.set_status("Connect an account first");
            return;
        };
        self.goto(Route::New { account, query: String::new() });
        self.fill_from_link();
    }

    /// Sends what has been written. Two calls, one job: what is on screen is
    /// written into its Gmail draft and that draft is sent — no dialog,
    /// because the key press that got here is the consent.
    ///
    /// An address still being typed becomes a chip first; text in a field that
    /// is not an address stops it, and so does nobody to send it to. While a
    /// save is on its way, the send waits for it, so its draft is the one sent.
    fn send_reply(&mut self) {
        if !self.nav.top().writing() || self.screen.sending {
            return;
        }
        let s = &mut self.screen;
        for (field, typing) in [&mut s.to, &mut s.cc, &mut s.bcc].into_iter().zip(s.typing.iter_mut()) {
            crate::ui::compose::commit(field, typing);
        }
        if let Some(problem) = self.unsendable() {
            self.set_status(problem);
            return;
        }
        if self.screen.saving {
            self.screen.then = Some(crate::state::Then::Send);
            return;
        }
        self.send_now();
    }

    /// Why what is on screen cannot go yet, if it cannot.
    fn unsendable(&self) -> Option<String> {
        let s = &self.screen;
        if let Some(typed) = s.typing.iter().map(|t| t.trim()).find(|t| !t.is_empty()) {
            return Some(format!("\"{typed}\" is not an email address."));
        }
        if crate::ui::compose::chips(&s.to).is_empty() {
            return Some("Add who it goes to first.".to_string());
        }
        // A forward goes without a note; anything else says something.
        match self.nav.top() {
            Route::Forward { .. } => None,
            Route::Reply { .. } if s.compose.trim().is_empty() => {
                Some("Write something first.".to_string())
            }
            _ if s.compose.trim().is_empty() && s.subject.trim().is_empty() => {
                Some("Write something first.".to_string())
            }
            _ => None,
        }
    }

    /// Writes what is being written into Gmail's Drafts once typing has
    /// paused, and asks to be woken for it.
    fn autosave(&mut self, ctx: &Context) {
        let s = &self.screen;
        if !self.nav.top().writing() || !s.dirty || s.saving || s.sending {
            return;
        }
        let wait = SAVE_AFTER - (ctx.input(|input| input.time) - s.edited);
        if wait > 0.0 {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
            return;
        }
        self.save_now();
    }

    /// Enter: what the keyboard is on, opened — a stop of the sidebar, a row
    /// of the list — or, in a conversation, the message it is on, opened or
    /// closed again.
    fn open_focused(&mut self) {
        match self.pane() {
            Pane::Sidebar => self.open_side(),
            Pane::List => self.open_listed(),
            Pane::Reader => {
                if self.nav.top().message().is_some() {
                    self.toggle_message();
                }
            }
        }
    }

    /// Opens one line of a list: its conversation — or, in the Drafts folder,
    /// the draft, in the new-email screen.
    pub fn open_row(&mut self, account: &str, thread: &crate::client::Thread) {
        match thread.latest.draft_id.as_str() {
            "" => self.open(account, &thread.id),
            draft => self.goto(Route::Draft {
                account: account.to_string(),
                draft: draft.to_string(),
            }),
        }
    }

    /// Where the keyboard is, as the screen has it: a list that is not on
    /// screen hands the keys to the reader, and a reader with nothing in it
    /// hands them to the list.
    pub fn pane(&self) -> Pane {
        match self.pane {
            Pane::List if self.list_step().is_none() => Pane::Reader,
            Pane::Reader if matches!(self.nav.top(), Route::Folder { .. } | Route::Settings) => {
                Pane::List
            }
            pane => pane,
        }
    }

    /// The step whose list is on screen: the list on top, or — on a wide
    /// window — the one beside the message or the account that is open.
    pub fn list_step(&self) -> Option<u64> {
        match self.nav.top() {
            Route::Folder { .. } | Route::Settings => Some(self.nav.key()),
            Route::Message { .. } | Route::Account { .. } if self.layout.list.is_some() => {
                self.nav.below_key()
            }
            _ => None,
        }
    }

    /// Left: one column towards the sidebar. From a reader with no list
    /// beside it, that is Back — the list is the screen before it.
    fn left(&mut self) {
        match self.pane() {
            Pane::Sidebar => {}
            Pane::List => {
                self.side_focus = self.side_here();
                self.side_follow = true;
                self.pane = Pane::Sidebar;
            }
            Pane::Reader => {
                if self.list_step().is_none() {
                    self.back();
                }
                self.pane = Pane::List;
            }
        }
    }

    /// Right: one column towards the reader, opening nothing — what is open
    /// stays open, and the keyboard's place in the column it left stays where
    /// it was. Enter is what opens. From a list with nothing open beside it,
    /// the row the keyboard is on is opened, so there is a reader to go into.
    fn right(&mut self) {
        match self.pane() {
            Pane::Sidebar => self.pane = Pane::List,
            Pane::List => match self.nav.top() {
                Route::Message { .. } | Route::Account { .. } => self.pane = Pane::Reader,
                _ => self.open_listed(),
            },
            Pane::Reader => {}
        }
    }

    /// The list's keyboard row, opened: a conversation, a draft, an account's
    /// settings — or Add account, on the stop after the last account.
    fn open_listed(&mut self) {
        let Some(step) = self.list_step() else {
            return;
        };
        let Some((route, screen)) = self.step_of(Some(step)) else {
            return;
        };
        match route.clone() {
            Route::Settings => {
                let focus = screen.account_focus;
                let selected = self.accounts.as_ref().and_then(|list| list.get(focus)).cloned();
                match selected {
                    Some(account) => {
                        let route = Route::Account { account };
                        if matches!(self.nav.top(), Route::Account { .. }) {
                            self.replace(route);
                        } else {
                            self.goto(route);
                        }
                        self.pane = Pane::Reader;
                    }
                    None if !self.connecting => self.start(Job::AddAccount),
                    None => {}
                }
            }
            Route::Folder { account, .. } => {
                let thread = screen
                    .mailbox
                    .as_ref()
                    .and_then(|mailbox| mailbox.threads.get(screen.focus))
                    .cloned();
                if let Some(thread) = thread {
                    self.open_row(&account, &thread);
                    self.pane = Pane::Reader;
                }
            }
            _ => {}
        }
    }

    /// The sidebar's stops for the keyboard, in the order they are drawn: the
    /// accounts, then the folders of the account on screen — each as what a
    /// click on it opens.
    pub fn side_stops(&self) -> Vec<Route> {
        let accounts = self.accounts.clone().unwrap_or_default();
        let mut stops: Vec<Route> = accounts.iter().map(|a| Route::inbox(a.as_str())).collect();
        if let Some(account) = self.account()
            && let Some(labels) = self.folders.get(account)
        {
            stops.extend(labels.iter().map(|label| Route::Folder {
                account: account.to_string(),
                folder: label.id.clone(),
            }));
        }
        stops
    }

    /// The sidebar stop of what is on screen: its folder, or else its account.
    fn side_here(&self) -> usize {
        let accounts = self.accounts.clone().unwrap_or_default();
        let account = self.account();
        if let (Some(account), Some(folder)) = (account, self.nav.top().folder())
            && let Some(j) = self
                .folders
                .get(account)
                .and_then(|labels| labels.iter().position(|label| label.id == folder))
        {
            return accounts.len() + j;
        }
        account
            .and_then(|account| accounts.iter().position(|a| a == account))
            .unwrap_or(0)
    }

    /// Moves the sidebar's keyboard stop, and asks it into view.
    fn move_side(&mut self, by: i32) {
        let last = self.side_stops().len() as i32 - 1;
        if last < 0 {
            return;
        }
        let next = (self.side_focus as i32 + by).clamp(0, last) as usize;
        if next != self.side_focus {
            self.side_focus = next;
            self.side_follow = true;
        }
    }

    /// The sidebar's keyboard stop, opened: an account's inbox, or a folder.
    /// The keyboard goes on into its list.
    fn open_side(&mut self) {
        let Some(route) = self.side_stops().get(self.side_focus).cloned() else {
            return;
        };
        self.goto(route);
        self.pane = Pane::List;
    }

    /// Opens one conversation. Showing it is what marks its unread messages
    /// read. `id` is the conversation's, or any message on it.
    ///
    /// Picked from the list beside an open message, it takes that message's
    /// place: Back goes up one level, to the list, not through every message
    /// looked at.
    pub fn open(&mut self, account: &str, id: &str) {
        let route = Route::Message {
            account: account.to_string(),
            folder: self.folder(),
            id: id.to_string(),
        };
        if matches!(self.nav.top(), Route::Message { .. }) {
            self.replace(route);
        } else {
            self.goto(route);
        }
        self.pane = Pane::Reader;
    }

    /// Down and Up: the next line in a list, the next message in a
    /// conversation. The same two keys, because in every view they mean
    /// "further on".
    fn step(&mut self, by: i32) {
        // In a reply the keys are the editor's, whether or not it has focus.
        if self.nav.top().writing() {
            return;
        }
        match self.pane() {
            Pane::Sidebar => self.move_side(by),
            Pane::List => self.move_focus(by),
            // Arrows read the message: they scroll. The conversation is
            // walked by clicking its lines.
            Pane::Reader => {
                if self.nav.top().message().is_some() {
                    self.body_scroll += by as f32 * BODY_STEP;
                }
            }
        }
    }

    /// PageDown and PageUp: a screenful of the column the keyboard is in — of
    /// the list, or of the open message's body. Walking a conversation is the
    /// arrows' job; these are how the body under them moves without a mouse.
    fn page(&mut self, by: i32) {
        if self.nav.top().writing() {
            return;
        }
        match self.pane() {
            Pane::Reader if self.nav.top().message().is_some() => {
                self.body_scroll += by as f32 * BODY_STEP;
            }
            Pane::Reader => {}
            _ => self.step(by),
        }
    }

    /// Walks the conversation on screen, message by message, and asks the list
    /// to bring the one it landed on back into view.
    pub fn step_conversation(&mut self, by: i32) {
        let moved = self
            .screen.conversation
            .as_mut()
            .is_some_and(|conversation| conversation.step(by));
        if moved {
            self.follow_focus = true;
        }
    }

    /// Enter on a message of the conversation: opens it, or closes it when it
    /// is the one already open. Only ever one is open, so the body pane always
    /// has exactly one message to draw.
    ///
    /// The body opens under that message's own line, so the line is brought
    /// into view: expanding one at the bottom of a long conversation must not
    /// open it off screen.
    pub fn toggle_message(&mut self) {
        if let Some(conversation) = &mut self.screen.conversation {
            conversation.toggle();
        }
        self.follow_focus = true;
        self.show_open_message();
    }

    /// Moves the keyboard's stop in the account list, and asks it to bring
    /// the stop it landed on back into view.
    pub fn move_account_focus(&mut self, by: i32) {
        self.move_focus(by);
    }

    /// Moves the keyboard's row in the list on screen — on top, or beside the
    /// message or account open — and asks the list to bring it into view: a
    /// conversation, or on the settings list an account, with Add account on
    /// the end.
    ///
    /// Landing on the last conversation loaded — or pressing on past it — is
    /// the ask for the page after this one, which goes on the end.
    pub fn move_focus(&mut self, by: i32) {
        let Some(step) = self.list_step() else {
            return;
        };
        let stops = self.account_stops();
        let Some((route, screen)) = self.step_of(Some(step)) else {
            return;
        };
        let settings = matches!(route, Route::Settings);
        let (rows, at) = if settings {
            (stops, screen.account_focus)
        } else {
            (screen.mailbox.as_ref().map_or(0, |m| m.threads.len()), screen.focus)
        };
        if rows == 0 {
            return;
        }
        let next = (at as i32).saturating_add(by).clamp(0, rows as i32 - 1) as usize;
        if settings {
            screen.account_focus = next;
        } else {
            screen.focus = next;
        }
        if !settings && by > 0 && next == rows - 1 {
            self.load_more_for(step);
        }
        if next != at {
            self.follow_focus = true;
        }
    }

    /// End: the last stop of the column the keyboard is in — the last line
    /// loaded, and the page after it. In a conversation the key is the body's.
    fn to_end(&mut self) {
        if self.nav.top().writing() {
            return;
        }
        match self.pane() {
            Pane::Sidebar => self.move_side(i32::MAX),
            Pane::List => {
                self.move_focus(i32::MAX);
                if let Some(step) = self.list_step() {
                    self.load_more_for(step);
                }
            }
            Pane::Reader => {}
        }
    }

    /// Trashes the message the keyboard is on in a conversation, or the whole
    /// conversation the keyboard is on in the list. Nothing at all while a
    /// reply is being written: there is no message under this view.
    fn delete_current(&mut self) {
        let Some(account) = self.account().map(str::to_owned) else {
            return;
        };
        if self.nav.top().writing() {
            return;
        }
        let ids: Vec<String> = match self.pane() {
            Pane::Reader if self.nav.top().message().is_some() => self
                .screen.conversation
                .as_ref()
                .and_then(|thread| thread.focused())
                .map(|message| vec![message.id.clone()])
                .unwrap_or_default(),
            // In the list, the row the keyboard is on — beside an open
            // message too.
            Pane::List => {
                let step = self.list_step();
                match self.step_of(step) {
                    Some((_, screen)) => screen
                        .mailbox
                        .as_ref()
                        .and_then(|mailbox| mailbox.threads.get(screen.focus))
                        .map(|thread| thread.ids.clone())
                        .unwrap_or_default(),
                    None => Vec::new(),
                }
            }
            _ => Vec::new(),
        };
        for id in ids {
            self.delete(&account, &id);
        }
    }

    /// Trashes one message, named. The row buttons come through here.
    pub fn delete(&mut self, account: &str, id: &str) {
        self.start(Job::Delete {
            account: account.to_string(),
            id: id.to_string(),
        });
    }

    /// Puts the unread flag back on the message the keyboard is on.
    fn mark_unread(&mut self) {
        if self.nav.top().message().is_none() {
            return;
        }
        let (Some(account), Some(id)) = (
            self.account().map(str::to_owned),
            self.screen.conversation
                .as_ref()
                .and_then(|thread| thread.focused())
                .map(|message| message.id.clone()),
        ) else {
            return;
        };
        self.start(Job::Mark {
            account,
            id,
            read: false,
        });
    }

    /// Starts whatever the screen on top still needs. One job per screen: a
    /// refusal is said once, not retried every frame, and a screen Back
    /// returned to already has what it needs.
    fn ensure_loaded(&mut self) {
        if self.jobs.busy() {
            return;
        }
        // The sidebar lists the accounts on every screen.
        if self.accounts.is_none() && !self.accounts_asked {
            self.accounts_asked = true;
            self.start(Job::Accounts);
            return;
        }
        if !self.screen.asked {
            self.screen.asked = true;
            if let Some(job) = self.screen_job() {
                self.start(job);
                return;
            }
        }
        // The folders and their counts: the account on screen first, then
        // the rest, one at a time behind whatever the reader is doing.
        let next = self
            .account()
            .map(str::to_owned)
            .into_iter()
            .chain(self.accounts.iter().flatten().cloned())
            .find(|account| !self.folders_asked.contains(account));
        if let Some(account) = next {
            self.folders_asked.insert(account.clone());
            self.start(Job::Labels { account });
        }
    }

    /// What the screen on top still needs from the provider, if anything.
    fn screen_job(&self) -> Option<Job> {
        match self.nav.top() {
            Route::Settings => None,
            Route::Folder { account, folder } => self.screen.mailbox.is_none().then(|| Job::List {
                account: account.clone(),
                folder: folder.clone(),
                unread: self.screen.unread,
                page: None,
            }),
            Route::Message { account, id, .. } => (!self.showing(id)).then(|| Job::Thread {
                account: account.clone(),
                id: id.clone(),
            }),
            // A reply with nothing in it yet: drafted only to read the address
            // and the subject off the message being answered. Nothing is sent,
            // and the body that replaces it is the one the reader types.
            Route::New { .. } => None,
            // A reply to the sender leaves the others off; a reply to all
            // copies everyone the message was to or copied to.
            Route::Reply { account, id, all, .. } => self.screen.draft.is_none().then(|| Job::Draft {
                account: account.clone(),
                id: id.clone(),
                body: String::new(),
                attach: Vec::new(),
                to: None,
                cc: if *all { None } else { Some(String::new()) },
                bcc: None,
                draft_id: None,
            }),
            // A Gmail draft: its fields and files, to write on.
            Route::Draft { account, draft } => self.screen.draft_id.is_none().then(|| Job::ReadDraft {
                account: account.clone(),
                draft_id: draft.clone(),
            }),
            // A forward with nothing in it yet: asked for only to read the
            // header, the subject and the files off. Nothing is written.
            Route::Forward { account, id, .. } => self.screen.draft.is_none().then(|| Job::Forward {
                account: account.clone(),
                id: id.clone(),
                body: String::new(),
                subject: None,
                to: None,
                cc: None,
                bcc: None,
                attach: None,
                draft_id: None,
            }),
            Route::Account { .. } => None,
        }
    }

    /// The title bar says where we are, and what is open.
    fn update_title(&mut self, ctx: &Context) {
        let title = match self.nav.top() {
            Route::Settings => "Post".to_string(),
            Route::Account { account } => format!("Post — {}", self.name_of(account)),
            Route::Folder { account, .. } => format!("Post — {}", self.name_of(account)),
            Route::Message { account, .. } => match &self.screen.conversation {
                Some(thread) if !thread.subject.is_empty() => {
                    format!("Post — {}", thread.subject)
                }
                _ => format!("Post — {account}"),
            },
            Route::Reply { account, .. } => match &self.screen.draft {
                Some(draft) if !draft.subject.is_empty() => format!("Post — {}", draft.subject),
                _ => format!("Post — reply from {account}"),
            },
            Route::New { account, .. } | Route::Draft { account, .. } => {
                match self.screen.subject.trim() {
                    "" => format!("Post — new message from {account}"),
                    subject => format!("Post — {subject}"),
                }
            }
            Route::Forward { account, .. } => match self.screen.subject.trim() {
                "" => format!("Post — forward from {account}"),
                subject => format!("Post — {subject}"),
            },
        };
        if title != self.last_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }
}


/// Hyprland does not let a window bring itself forward, so it is asked to
/// focus this process's window: it switches to the window's workspace and
/// raises it over whatever covers it. Elsewhere this does nothing.
fn hyprland_front() {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_none() {
        return;
    }
    let window = format!("pid:{}", std::process::id());
    std::thread::spawn(move || {
        let _ = std::process::Command::new("hyprctl")
            .args(["dispatch", "focuswindow", &window])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::INBOX;
    use crate::client::{Done, Mailbox};
    use serde_json::json;

    /// A mailbox of `rows` messages — more than fit on a screen, which is the
    /// only interesting case for scrolling.
    fn listing(rows: usize) -> Mailbox {
        let messages: Vec<_> = (0..rows)
            .map(|i| {
                json!({
                    "id": format!("18f{i}"),
                    "thread_id": format!("18f{i}"),
                    "from": "Ale <ale@acme.com>",
                    "subject": "Re: the quote",
                    "date": "2025-09-12 10:33",
                    "unread": true
                })
            })
            .collect();
        Mailbox::of_answer(
            "ed@acme.com",
            &json!({"account": "ed@acme.com", "messages": messages}),
        )
    }

    fn inbox(rows: usize) -> PostApp {
        let mut app = PostApp::new(Route::inbox("ed@acme.com".to_string()));
        app.accounts = Some(vec!["ed@acme.com".to_string()]);
        // Its folders are read already: what a test asks for is its own.
        app.folders_asked.insert("ed@acme.com".to_string());
        app.absorb(Done::List(listing(rows)));
        app
    }

    /// Down and Up move the selection, and ask the list to bring the row they
    /// landed on back into view. The ends of the list are ends: the selection
    /// stays, and nothing is asked of the list.
    #[test]
    fn the_arrows_move_the_selection_and_the_list_follows_it() {
        let mut app = inbox(40);
        assert_eq!(app.screen.focus, 0);
        assert!(!app.follow_focus, "a fresh list is where it should be");

        app.move_focus(1);
        assert_eq!(app.screen.focus, 1);
        assert!(app.follow_focus, "the list follows the selection");

        // The frame that scrolls takes the flag with it, so a mouse scrolling
        // away afterwards is left where the reader put it.
        app.follow_focus = false;
        app.move_focus(1);
        assert_eq!(app.screen.focus, 2);
        assert!(app.follow_focus);

        app.screen.focus = 0;
        app.follow_focus = false;
        app.move_focus(-1);
        assert_eq!(app.screen.focus, 0, "the top is the top");
        assert!(!app.follow_focus, "nothing moved, so nothing follows");

        app.screen.focus = 39;
        app.move_focus(1);
        assert_eq!(app.screen.focus, 39, "and the bottom is the bottom");
        assert!(!app.follow_focus);
    }

    /// An empty list has nothing to select and nothing to scroll.
    #[test]
    fn the_arrows_do_nothing_to_an_empty_list() {
        let mut app = inbox(0);
        app.move_focus(1);
        assert_eq!(app.screen.focus, 0);
        assert!(!app.follow_focus);
    }

    // --------------------------------------------------------------- replying

    /// A reply, exactly as Post's own library addresses one.
    fn draft() -> crate::client::Draft {
        crate::client::Draft {
            id: "reply-18f7".to_string(),
            to: "ale@acme.com".to_string(),
            cc: "cm@ey.com".to_string(),
            subject: "Re: the quote".to_string(),
            attribution: "On 2025-09-12 10:33, Ale <ale@acme.com> wrote:".to_string(),
            quote: "Can you send the quote?".to_string(),
            ..Default::default()
        }
    }

    /// A conversation of three messages, exactly as `post_thread` answers it:
    /// the message asked for, a reply of this account's own, and the newest.
    fn conversation(id: &str) -> crate::client::Conversation {
        crate::client::Conversation::of_answer(
            "ed@acme.com",
            &json!({
                "account": "ed@acme.com",
                "thread_id": id,
                "subject": "Re: the quote",
                "messages": [
                    {"id": format!("{id}-a"), "from": "Ale <ale@acme.com>",
                     "date": "2025-09-10 19:20", "unread": false, "text": "The quote?"},
                    {"id": format!("{id}-b"), "from": "Ed <ed@acme.com>",
                     "date": "2025-09-12 09:10", "unread": false, "text": "On its way."},
                    {"id": id, "from": "Ale <ale@acme.com>",
                     "date": "2025-09-12 10:33", "unread": true, "text": "Looks good."}
                ]
            }),
        )
    }

    /// A conversation open on screen.
    fn reading(id: &str) -> PostApp {
        let mut app = inbox(40);
        app.open("ed@acme.com", id);
        app.absorb(Done::Thread(conversation(id)));
        app
    }

    /// A conversation open, and Reply pressed on it.
    fn replying() -> PostApp {
        let mut app = reading("18f7");
        app.reply(false);
        app
    }

    /// Reply puts the editor over the message, empty, with the message behind
    /// it: the body pane is gone and Back goes to what is being answered.
    #[test]
    fn reply_opens_an_empty_editor_over_the_message() {
        let app = replying();
        assert_eq!(
            app.nav.top(),
            &Route::Reply { account: "ed@acme.com".to_string(), folder: INBOX.to_string(), id: "18f7".to_string(), all: false }
        );
        assert_eq!(app.nav.depth(), 3, "the inbox, the message, the reply");
        assert!(app.screen.compose.is_empty(), "a reply starts empty");
        assert!(app.screen.draft.is_none(), "and unaddressed until the draft lands");
        assert!(!app.screen.sending);
        // A reply is not the message: nothing here is the body pane's.
        assert!(app.nav.top().message().is_none());
        assert_eq!(app.nav.top().replying_to(), Some("18f7"));

        // Nothing to reply to is not a reply: from the list, Reply does
        // nothing rather than composing to whatever was selected.
        let mut list = inbox(40);
        list.reply(false);
        assert_eq!(
            list.nav.top(),
            &Route::inbox("ed@acme.com".to_string())
        );
    }

    /// The draft says who the reply goes to and what it is about. It does not
    /// touch what has been typed: the editor owns the body.
    #[test]
    fn the_draft_addresses_the_reply_without_touching_it() {
        let mut app = replying();
        app.screen.compose = "Looks good — send it.".to_string();
        app.absorb(Done::Drafted(draft()));

        let addressed = app.screen.draft.as_ref().expect("a draft");
        assert_eq!(addressed.to, "ale@acme.com");
        assert_eq!(addressed.subject, "Re: the quote");
        assert_eq!(app.screen.compose, "Looks good — send it.", "untouched");
        assert!(!app.screen.sending, "nothing was asked to be sent");
        assert!(!app.jobs.busy(), "and nothing was started");

        // The reply answers everybody, and the fields say so — they are the
        // reader's from here.
        assert_eq!(app.screen.to, "ale@acme.com");
        assert_eq!(app.screen.cc, "cm@ey.com");
    }

    /// The addresses are filled once. What the reader types over them is what
    /// is sent: a draft that lands afterwards does not write over them.
    #[test]
    fn the_address_fields_are_the_readers_once_they_are_filled() {
        let mut app = replying();
        app.absorb(Done::Drafted(draft()));
        app.screen.to = "fredrik@norberg.se".to_string();
        app.screen.cc.clear();
        app.screen.compose = "Ses där.".to_string();

        app.send_reply();
        assert!(app.screen.sending);

        app.absorb(Done::Drafted(draft()));
        assert_eq!(app.screen.to, "fredrik@norberg.se", "not written back over");
        assert!(app.screen.cc.is_empty());
    }

    /// Sending is one errand: the draft is rewritten with what was typed, and
    /// that draft is what goes. An empty reply is not sent at all.
    #[test]
    fn sending_writes_the_typed_body_down_first() {
        let mut app = replying();
        app.absorb(Done::Drafted(draft()));

        app.set_status("");
        app.send_reply();
        assert!(!app.screen.sending, "an empty reply is not a reply");
        assert!(!app.jobs.busy());
        assert!(app.status.contains("Write something"), "got {}", app.status);

        app.screen.compose = "Looks good — send it.".to_string();
        app.send_reply();
        assert!(app.screen.sending);
        assert_eq!(app.status, "Sending…");
        assert!(app.jobs.busy(), "on its way");

        // A second press while it is on its way sends nothing more.
        app.set_status("");
        app.send_reply();
        assert_eq!(app.status, "");
    }

    /// Sent: the editor empties and the reader is back at the message they
    /// answered, with one line saying where it went.
    #[test]
    fn a_sent_reply_goes_back_to_the_message() {
        let mut app = replying();
        app.screen.compose = "Looks good.".to_string();
        app.screen.sending = true;
        app.absorb(Done::Sent {
            to: "ale@acme.com".to_string(),
        });

        assert!(!app.screen.sending);
        assert!(app.screen.compose.is_empty(), "nothing is left in the editor");
        assert!(app.screen.draft.is_none());
        assert_eq!(
            app.nav.top(),
            &Route::Message { account: "ed@acme.com".to_string(), folder: INBOX.to_string(), id: "18f7".to_string() }
        );
        assert!(app.status.contains("ale@acme.com"), "got {}", app.status);
    }

    /// A reply that did not go keeps every word of itself: the reader can fix
    /// whatever it was and send again.
    #[test]
    fn a_reply_that_failed_is_still_written() {
        let mut app = replying();
        app.screen.compose = "Looks good.".to_string();
        app.screen.sending = true;
        app.absorb(Done::Failed {
            about: "Sending…".to_string(),
            error: "Gmail refused the call (403)".to_string(),
        });

        assert!(!app.screen.sending, "and the window is not left waiting");
        assert_eq!(app.screen.compose, "Looks good.");
        assert_eq!(
            app.nav.top(),
            &Route::Reply { account: "ed@acme.com".to_string(), folder: INBOX.to_string(), id: "18f7".to_string(), all: false },
            "still in the reply, where the words are"
        );
        assert!(app.status.contains("403"), "got {}", app.status);
    }

    /// Reply answers the sender and leaves the others off; Reply all asks for
    /// everyone the message was to or copied to.
    #[test]
    fn reply_answers_the_sender_and_reply_all_everyone() {
        let reply = replying();
        assert!(matches!(reply.screen_job(), Some(Job::Draft { cc: Some(ref cc), .. }) if cc.is_empty()));
        let mut all = reading("18f7");
        all.reply(true);
        assert_eq!(
            all.nav.top(),
            &Route::Reply { account: "ed@acme.com".to_string(), folder: INBOX.to_string(), id: "18f7".to_string(), all: true }
        );
        assert!(matches!(all.screen_job(), Some(Job::Draft { cc: None, .. })));
    }

    /// The keys that act on a message do nothing to a reply: there is no row
    /// under this view to trash, and no body of someone else's to scroll.
    #[test]
    fn the_message_keys_do_nothing_while_a_reply_is_open() {
        let mut app = replying();
        app.screen.focus = 7;

        app.step(1);
        assert_eq!(app.body_scroll, 0.0, "nothing to scroll here");
        assert_eq!(app.screen.focus, 7, "and nothing to select");

        app.to_end();
        assert_eq!(app.screen.focus, 7);

        app.set_status("");
        app.delete_current();
        assert!(!app.jobs.busy(), "nothing was trashed");
        assert_eq!(app.status, "");
    }

    /// In a conversation the arrows scroll the reading; the messages are
    /// walked by clicking their lines. The selection in the list behind
    /// stays exactly where the reader left it.
    #[test]
    fn in_a_conversation_the_arrows_scroll_the_reading() {
        let mut app = inbox(40);
        app.screen.focus = 7;
        app.open("ed@acme.com", "18f7");
        app.absorb(Done::Thread(conversation("18f7")));
        let thread = app.screen.conversation.as_ref().expect("a conversation");
        assert_eq!(thread.focus, 2, "opened at the newest");
        assert_eq!(thread.open, Some(2));

        app.step(1);
        let thread = app.screen.conversation.as_ref().expect("a conversation");
        assert!(app.body_scroll > 0.0, "down reads on");
        assert_eq!(thread.focus, 2, "the conversation's lines stay put");
        assert_eq!(thread.open, Some(2), "and what is open stays open");
        app.step(-1);
        assert_eq!(app.body_scroll, 0.0, "up reads back");

        app.exec(Command::Open, &eframe::egui::Context::default());
        assert!(app.message().is_none(), "Enter closes the open message");

        // The page keys are what moves a body without a mouse.
        app.page(1);
        assert!(app.body_scroll > 0.0, "page down scrolls down");
        let down = app.body_scroll;
        app.page(-1);
        assert_eq!(app.body_scroll, 0.0, "page up undoes it");
        // The frame that scrolls takes it; the next one starts from nothing.
        app.body_scroll = down;
        let taken = std::mem::take(&mut app.body_scroll);
        assert!(taken != 0.0);
        assert_eq!(app.body_scroll, 0.0);

        // Back in the list, the arrows move the selection again.
        app.back();
        app.step(1);
        assert_eq!(app.screen.focus, 8);
        assert_eq!(app.body_scroll, 0.0);
    }

    /// Reply answers the newest message on the conversation, whichever one the
    /// keyboard is resting on — that is what keeps it on one thread.
    #[test]
    fn reply_answers_the_newest_message_on_the_conversation() {
        let mut app = reading("18f7");
        app.step_conversation(-2);
        assert_eq!(
            app.screen.conversation.as_ref().expect("a conversation").focus,
            0,
            "the keyboard is on the oldest"
        );
        app.reply(false);
        assert_eq!(
            app.nav.top(),
            &Route::Reply { account: "ed@acme.com".to_string(), folder: INBOX.to_string(), id: "18f7".to_string(), all: false },
            "and the reply still answers the newest"
        );
    }

    // ---------------------------------------------------- every step its own

    /// Back shows the list exactly as it was left — its page and where the
    /// keyboard was — and asks the provider for nothing.
    #[test]
    fn back_shows_the_list_as_it_was_left_without_a_fetch() {
        let mut app = inbox(40);
        app.screen.focus = 12;
        app.open("ed@acme.com", "18f12");
        assert!(app.screen.mailbox.is_none(), "a conversation is a screen of its own");

        app.back();
        assert_eq!(app.screen.focus, 12);
        assert_eq!(app.rows().len(), 40);
        app.ensure_loaded();
        assert!(!app.jobs.busy(), "nothing was fetched");
    }

    /// A conversation that lands while the reader is somewhere else lands on
    /// the step that asked for it, and is there when they come back.
    #[test]
    fn a_load_lands_on_the_step_that_asked_for_it() {
        let mut app = inbox(40);
        app.open("ed@acme.com", "18f7");
        let asked = app.nav.key();
        app.goto(Route::Settings);

        app.answer(Some(asked), Done::Thread(conversation("18f7")));
        assert_eq!(app.nav.top(), &Route::Settings, "nobody was moved");
        assert!(app.screen.conversation.is_none(), "settings got nothing");

        app.back();
        assert_eq!(app.message().expect("a message").id, "18f7");
        // Reading it changed the folders' counts, which are read again
        // quietly; the conversation itself is not asked for again.
        app.set_status("");
        app.ensure_loaded();
        assert_eq!(app.status, "", "and it is not asked for again");
    }

    /// A message trashed from one screen leaves every screen: the open
    /// conversation, and the list behind it.
    #[test]
    fn a_trashed_message_leaves_every_step() {
        let mut app = inbox(40);
        app.open("ed@acme.com", "18f7");
        app.absorb(Done::Thread(conversation("18f7")));

        app.absorb(Done::Deleted {
            id: "18f7-a".to_string(),
        });
        let thread = app.screen.conversation.as_ref().expect("a conversation");
        assert_eq!(thread.messages.len(), 2, "gone from the conversation");

        app.absorb(Done::Deleted {
            id: "18f3".to_string(),
        });
        app.back();
        assert!(
            app.screen.mailbox.as_ref().expect("a page").row("18f3").is_none(),
            "and from the list behind it"
        );
        assert_eq!(app.rows().len(), 39);
    }

    /// A window opened straight at a reply has its conversation behind it:
    /// the reply that went goes back to it, asked for again so it is on it.
    #[test]
    fn a_reply_sent_from_a_window_opened_at_it_goes_back_to_its_conversation() {
        let mut app = PostApp::new(Route::Reply {
            account: "ed@acme.com".to_string(),
            folder: INBOX.to_string(),
            id: "18f7".to_string(),
            all: false,
        });
        app.screen.sending = true;
        app.absorb(Done::Sent {
            to: "ale@acme.com".to_string(),
        });
        assert_eq!(
            app.nav.top(),
            &Route::Message {
                account: "ed@acme.com".to_string(),
                folder: INBOX.to_string(),
                id: "18f7".to_string()
            }
        );
        assert_eq!(app.nav.depth(), 2, "the folder, the conversation");
        assert!(app.screen.conversation.is_none() && !app.screen.asked, "asked for again");
        assert!(app.status.contains("Reply sent"), "got {}", app.status);
    }

    /// A new message that went from a window opened straight at it goes to
    /// that account's mail, and Back does not return to an empty editor.
    #[test]
    fn a_new_message_sent_from_a_window_opened_at_it_goes_to_the_inbox() {
        let mut app = PostApp::new(Route::New {
            account: "ed@acme.com".to_string(),
            query: String::new(),
        });
        app.screen.sending = true;
        app.absorb(Done::Sent {
            to: "ale@acme.com".to_string(),
        });
        assert_eq!(app.nav.top(), &Route::inbox("ed@acme.com"));
        assert_eq!(app.nav.depth(), 1);
        assert!(app.status.contains("Sent to ale@acme.com"), "got {}", app.status);
    }

    /// From the list on top a click is a step; from the list beside an open
    /// message it takes that message's place, so Back goes up one level.
    #[test]
    fn a_message_picked_beside_another_takes_its_place() {
        let mut app = inbox(40);
        app.open("ed@acme.com", "18f3");
        assert_eq!(app.nav.depth(), 2);
        app.open("ed@acme.com", "18f4");
        assert_eq!(app.nav.depth(), 2, "no step for each message looked at");
        assert_eq!(app.nav.top().message(), Some("18f4"));
        app.back();
        assert_eq!(app.nav.top(), &Route::inbox("ed@acme.com"));
    }

    // ---------------------------------------------------- folders and search

    /// A page the library refreshed by itself is the inbox's: it lands on the
    /// inbox showing everything, and on no other folder.
    #[test]
    fn a_refreshed_inbox_page_reaches_only_the_inbox() {
        let mut app = inbox(3);
        app.goto(Route::Folder {
            account: "ed@acme.com".to_string(),
            folder: "SENT".to_string(),
        });
        let mut sent = listing(2);
        sent.folder = "SENT".to_string();
        app.absorb(Done::List(sent));

        app.answer(None, Done::List(listing(5)));
        assert_eq!(app.rows().len(), 2, "the sent folder is not the inbox");
        app.back();
        assert_eq!(app.rows().len(), 5, "the inbox took it");
    }

    /// Unread lists the folder again with only what is unread in it, and a
    /// refreshed page of the whole inbox does not land on it.
    #[test]
    fn unread_lists_the_folder_again() {
        let mut app = inbox(3);
        let step = app.nav.key();
        app.filter(step, "ed@acme.com", INBOX, true);
        assert!(app.screen.unread);
        assert!(app.screen.mailbox.is_none(), "listed again");
        assert!(app.jobs.busy());

        app.answer(None, Done::List(listing(5)));
        assert!(app.screen.mailbox.is_none(), "the whole inbox is not Unread");
    }

    /// The folders are read behind the screen, quietly, and read again when
    /// a message in them changes; the old counts stay until the new ones land.
    #[test]
    fn the_folders_are_counted_behind_the_screen() {
        let mut app = inbox(3);
        app.folders_asked.clear();
        app.screen.asked = true;
        app.set_status("");
        app.ensure_loaded();
        assert!(app.folders_asked.contains("ed@acme.com"));
        assert!(app.jobs.busy());
        assert_eq!(app.status, "", "quietly");

        app.absorb(Done::Labels {
            account: "ed@acme.com".to_string(),
            labels: vec![crate::client::Label {
                id: INBOX.to_string(),
                name: "Inbox".to_string(),
                system: true,
                unread: 3,
                total: 40,
            }],
        });
        app.absorb(Done::Marked {
            id: "18f1".to_string(),
            read: true,
        });
        assert!(!app.folders_asked.contains("ed@acme.com"), "counted again");
        assert_eq!(app.folders["ed@acme.com"][0].unread, 3);
    }

    /// A search is opened from the box as a folder of its own, and what is
    /// opened from it has the search behind it.
    #[test]
    fn a_search_is_a_folder_of_its_own() {
        let mut app = inbox(3);
        let found = Route::Folder {
            account: "ed@acme.com".to_string(),
            folder: crate::route::searching("from:ale"),
        };
        app.goto(found.clone());
        assert_eq!(app.nav.depth(), 2);
        app.open("ed@acme.com", "18f1");
        assert_eq!(app.nav.below_mut().map(|(_, route, _)| route.clone()), Some(found));
    }

    // ------------------------------------------------------------ writing

    /// A new email, open in the window.
    fn writing() -> PostApp {
        let mut app = inbox(3);
        app.new_message();
        app
    }

    /// What is typed is saved into Gmail's Drafts quietly, and the id it comes
    /// back under is the draft every later save writes over.
    #[test]
    fn what_is_typed_is_saved_as_one_gmail_draft() {
        let mut app = writing();
        app.screen.compose = "Hej".to_string();
        app.screen.dirty = true;
        app.set_status("");
        app.save_now();
        assert!(app.screen.saving && !app.screen.dirty);
        assert!(app.jobs.busy());
        assert_eq!(app.status, "", "quietly");

        app.absorb(Done::Written(crate::client::Draft {
            id: "r-1".to_string(),
            ..Default::default()
        }));
        assert!(!app.screen.saving);
        assert_eq!(app.screen.draft_id.as_deref(), Some("r-1"));
        assert!(matches!(
            app.writing_job(),
            Some(Job::New { draft_id: Some(id), .. }) if id == "r-1"
        ));
    }

    /// Send pressed while a save is on its way waits for it, so the draft
    /// that save writes is the one sent — never a second one beside it.
    #[test]
    fn send_waits_for_the_save_on_its_way() {
        let mut app = writing();
        app.screen.to = "ale@acme.com".to_string();
        app.screen.compose = "Hej".to_string();
        app.screen.saving = true;
        app.send_reply();
        assert!(!app.screen.sending, "not yet");
        assert_eq!(app.screen.then, Some(crate::state::Then::Send));

        app.absorb(Done::Written(crate::client::Draft {
            id: "r-1".to_string(),
            ..Default::default()
        }));
        assert!(app.screen.sending, "now, with the draft's id");
        assert_eq!(app.screen.draft_id.as_deref(), Some("r-1"));
    }

    /// An address still being typed becomes a chip on Send; text that is not an
    /// address stops it, and says so.
    #[test]
    fn send_refuses_what_is_not_an_address() {
        let mut app = writing();
        app.screen.compose = "Hej".to_string();
        app.screen.typing[0] = "ale@acme.com".to_string();
        app.screen.typing[1] = "fredrik".to_string();
        app.screen.copies = true;
        app.send_reply();
        assert_eq!(app.screen.to, "ale@acme.com", "the finished one is a chip");
        assert!(!app.screen.sending);
        assert!(app.status.contains("fredrik"), "got {}", app.status);
    }

    /// Discard deletes the Gmail draft and leaves, saving nothing on the way.
    #[test]
    fn discard_deletes_the_draft_and_saves_nothing() {
        let mut app = writing();
        app.screen.compose = "Hej".to_string();
        app.screen.draft_id = Some("r-1".to_string());
        app.screen.dirty = true;
        app.set_status("");
        app.discard();
        assert_eq!(app.nav.top(), &Route::inbox("ed@acme.com"), "back where it came from");
        assert_eq!(app.status, Job::DeleteDraft {
            account: "ed@acme.com".to_string(),
            draft_id: "r-1".to_string(),
        }
        .about());
    }

    /// Leaving a screen with unsaved typing on it saves it into Drafts first.
    #[test]
    fn leaving_unsaved_typing_saves_it() {
        let mut app = writing();
        app.screen.compose = "Hej".to_string();
        app.screen.dirty = true;
        app.back();
        assert_eq!(app.nav.top(), &Route::inbox("ed@acme.com"));
        assert!(app.jobs.busy(), "saved on the way out");
    }

    /// A Gmail draft opens in the new-email screen with everything in it, and
    /// a row of the Drafts folder opens it there.
    #[test]
    fn a_gmail_draft_opens_to_be_written_on() {
        let mut app = inbox(3);
        let mut row = app.threads()[0].clone();
        row.latest.draft_id = "r-9".to_string();
        app.open_row("ed@acme.com", &row);
        assert_eq!(
            app.nav.top(),
            &Route::Draft {
                account: "ed@acme.com".to_string(),
                draft: "r-9".to_string()
            }
        );
        app.absorb(Done::Opened(crate::client::Draft {
            id: "r-9".to_string(),
            to: "ale@acme.com".to_string(),
            bcc: "fredrik@norberg.se".to_string(),
            subject: "Offerten".to_string(),
            body: "Hej".to_string(),
            ..Default::default()
        }));
        assert_eq!(app.screen.to, "ale@acme.com");
        assert!(app.screen.copies, "Bcc is on it, so it is on screen");
        assert_eq!(app.screen.compose, "Hej");
        assert_eq!(app.screen.draft_id.as_deref(), Some("r-9"));
        assert!(!app.screen.dirty, "nothing typed yet");
    }

    /// A draft opened from the Drafts folder and thrown away leaves that
    /// folder's list, behind the screen it was written on.
    #[test]
    fn a_discarded_draft_leaves_the_drafts_folder() {
        let mut app = inbox(1);
        app.goto(Route::Folder {
            account: "ed@acme.com".to_string(),
            folder: "DRAFT".to_string(),
        });
        app.absorb(Done::List(Mailbox::of_answer(
            "ed@acme.com",
            &json!({"account": "ed@acme.com", "label": "DRAFT", "messages": [
                {"id": "18f9", "thread_id": "18f9", "from": "Ed <ed@acme.com>",
                 "subject": "Offerten", "date": "2026-10-14 09:00", "draft_id": "r-9"}
            ]}),
        )));
        let row = app.threads()[0].clone();
        app.open_row("ed@acme.com", &row);
        app.screen.draft_id = Some("r-9".to_string());
        app.discard();
        app.absorb(Done::Discarded {
            draft_id: "r-9".to_string(),
        });
        assert_eq!(app.nav.top().folder(), Some("DRAFT"), "back in the folder");
        assert!(app.rows().is_empty(), "and the draft is not in it");
    }

    // --------------------------------------------------------- forwarding

    /// Forward takes the message being read, and opens with its subject and
    /// its files; it can go with no note at all.
    #[test]
    fn a_forward_opens_with_the_message_and_its_files() {
        let mut app = reading("18f7");
        app.forward();
        assert_eq!(
            app.nav.top(),
            &Route::Forward {
                account: "ed@acme.com".to_string(),
                folder: INBOX.to_string(),
                id: "18f7".to_string()
            }
        );
        app.absorb(Done::Drafted(crate::client::Draft {
            subject: "Fwd: the quote".to_string(),
            attachments: vec!["/tmp/offert.pdf".to_string()],
            attribution: "---------- Forwarded message ---------".to_string(),
            quote: "Looks good.".to_string(),
            ..Default::default()
        }));
        assert_eq!(app.screen.subject, "Fwd: the quote");
        assert_eq!(app.screen.attach, vec!["/tmp/offert.pdf".to_string()]);
        assert!(matches!(
            app.writing_job(),
            Some(Job::Forward { attach: Some(files), .. }) if files.len() == 1
        ));

        app.screen.to = "ale@acme.com".to_string();
        assert_eq!(app.unsendable(), None, "no note needed");
        app.back();
        assert_eq!(app.nav.top().message(), Some("18f7"), "Back goes up to the message");
    }

    // ----------------------------------------------------------- settings

    /// An account goes by its name everywhere once it has one; Enter on the
    /// settings list opens that account's settings.
    #[test]
    fn an_account_is_named_and_opened_from_settings() {
        let mut app = PostApp::new(Route::Settings);
        app.absorb(Done::Accounts(vec![
            ("ed@acme.com".to_string(), "Eduard Moldovan AB".to_string()),
            ("ale@acme.com".to_string(), "ale@acme.com".to_string()),
        ]));
        assert_eq!(app.name_of("ed@acme.com"), "Eduard Moldovan AB");
        assert_eq!(app.name_of("ale@acme.com"), "ale@acme.com");

        app.open_focused();
        assert_eq!(
            app.nav.top(),
            &Route::Account {
                account: "ed@acme.com".to_string()
            }
        );
        assert_eq!(app.nav.depth(), 2, "the settings list is behind it");

        app.absorb(Done::Renamed {
            account: "ed@acme.com".to_string(),
            name: "ed@acme.com".to_string(),
        });
        assert_eq!(app.name_of("ed@acme.com"), "ed@acme.com", "back to its address");
    }

    /// A removed account leaves nothing behind in the window either: it is off
    /// the list, its folders are gone, and the history starts again from
    /// settings.
    #[test]
    fn a_removed_account_leaves_nothing_behind() {
        let mut app = inbox(3);
        app.accounts = Some(vec!["ed@acme.com".to_string(), "ale@acme.com".to_string()]);
        app.folders.insert("ed@acme.com".to_string(), Vec::new());
        app.goto(Route::Account {
            account: "ed@acme.com".to_string(),
        });
        app.absorb(Done::Removed("ed@acme.com".to_string()));
        assert_eq!(app.accounts.as_deref(), Some(["ale@acme.com".to_string()].as_slice()));
        assert!(!app.folders.contains_key("ed@acme.com"));
        assert_eq!(app.nav.top(), &Route::Settings);
        assert_eq!(app.nav.depth(), 1);
    }

    // ------------------------------------------------------------ columns

    fn labels() -> Vec<crate::client::Label> {
        ["INBOX", "SENT", "Label_1"]
            .iter()
            .map(|id| crate::client::Label {
                id: id.to_string(),
                name: id.to_string(),
                system: id.chars().all(|c| c.is_ascii_uppercase()),
                unread: 0,
                total: 0,
            })
            .collect()
    }

    /// Left from the list goes to the sidebar, onto what is on screen; Up and
    /// Down walk the accounts and the folders; Right opens one and goes on
    /// into its list.
    #[test]
    fn left_reaches_the_sidebar_and_its_arrows_walk_it() {
        let mut app = inbox(3);
        app.accounts = Some(vec!["ed@acme.com".to_string(), "ale@acme.com".to_string()]);
        app.folders.insert("ed@acme.com".to_string(), labels());
        app.exec(Command::Left, &eframe::egui::Context::default());
        assert_eq!(app.pane(), Pane::Sidebar);
        assert_eq!(app.side_focus, 2, "the inbox, after the two accounts");

        app.step(1);
        assert_eq!(app.side_focus, 3, "down to Sent");
        app.exec(Command::Left, &eframe::egui::Context::default());
        assert_eq!(app.pane(), Pane::Sidebar, "the sidebar is the leftmost column");

        // Right only moves: the inbox stays on screen, the stop stays on Sent.
        app.exec(Command::Right, &eframe::egui::Context::default());
        assert_eq!(app.nav.top(), &Route::inbox("ed@acme.com"));
        assert_eq!(app.pane(), Pane::List);
        assert_eq!(app.side_focus, 3);

        // Enter is what opens it, and the keyboard goes on into its list.
        app.exec(Command::Left, &eframe::egui::Context::default());
        app.step(1);
        app.exec(Command::Open, &eframe::egui::Context::default());
        assert_eq!(
            app.nav.top(),
            &Route::Folder {
                account: "ed@acme.com".to_string(),
                folder: "SENT".to_string()
            }
        );
        assert_eq!(app.pane(), Pane::List, "and the keyboard went on into its list");

        // Up past the folders reaches the accounts.
        app.exec(Command::Left, &eframe::egui::Context::default());
        app.step(-3);
        assert_eq!(app.side_focus, 0, "the first account");
    }

    /// With a message open beside its list, Left puts the keyboard in the list:
    /// the arrows walk the list instead of the body. Right goes back to the
    /// message as it was; Enter opens the row in its place.
    #[test]
    fn left_from_the_reader_walks_the_list_beside_it() {
        let mut app = inbox(40);
        app.open("ed@acme.com", "18f3");
        app.absorb(Done::Thread(conversation("18f3")));
        assert_eq!(app.pane(), Pane::Reader);
        app.step(1);
        assert!(app.body_scroll > 0.0, "in the reader the arrows scroll");
        app.body_scroll = 0.0;

        app.exec(Command::Left, &eframe::egui::Context::default());
        assert_eq!(app.pane(), Pane::List);
        app.step(1);
        app.step(1);
        assert_eq!(app.body_scroll, 0.0, "the body stays where it is");
        let below = app.nav.below_key();
        assert_eq!(app.step_of(below).map(|(_, s)| s.focus), Some(2), "the list moved");

        app.exec(Command::Right, &eframe::egui::Context::default());
        assert_eq!(app.nav.top().message(), Some("18f3"), "Right opens nothing");
        assert_eq!(app.pane(), Pane::Reader);

        app.exec(Command::Left, &eframe::egui::Context::default());
        app.exec(Command::Open, &eframe::egui::Context::default());
        assert_eq!(app.nav.top().message(), Some("18f2"));
        assert_eq!(app.nav.depth(), 2, "in place of the message");
        assert_eq!(app.pane(), Pane::Reader);
    }

    /// Under 760 there is no list beside the message: Left is Back to it.
    #[test]
    fn left_from_a_narrow_reader_goes_back_to_the_list() {
        let mut app = inbox(3);
        app.layout = crate::layout::Layout::of(500.0);
        app.open("ed@acme.com", "18f1");
        app.exec(Command::Left, &eframe::egui::Context::default());
        assert_eq!(app.nav.top(), &Route::inbox("ed@acme.com"));
        assert_eq!(app.pane(), Pane::List);
    }

    /// The labels the palette offers where the app is, with the account's
    /// Inbox and Drafts in the sidebar.
    fn offered(app: &mut PostApp) -> Vec<String> {
        let folder = |id: &str, name: &str| crate::client::Label {
            id: id.to_string(),
            name: name.to_string(),
            system: true,
            unread: 0,
            total: 0,
        };
        app.folders.insert(
            "ed@acme.com".to_string(),
            vec![folder(INBOX, "Inbox"), folder("DRAFT", "Drafts")],
        );
        let ctx = eframe::egui::Context::default();
        app.palette_lines(&ctx).into_iter().map(|(choice, _)| choice.label).collect()
    }

    /// The palette offers what does something where the window is: Send and
    /// Attach while writing, Reply while reading, the filter on a list — and
    /// the folders and accounts to go to from anywhere.
    #[test]
    fn the_palette_offers_what_applies_where_the_window_is() {
        let mut app = writing();
        let here = offered(&mut app);
        for label in ["Send", "Attach files…", "Add Cc and Bcc", "Discard the draft", "Go to Inbox"] {
            assert!(here.iter().any(|l| l == label), "writing offers no {label}: {here:?}");
        }
        assert!(!here.iter().any(|l| l == "Reply"), "nothing to reply to while writing");

        let mut app = reading("t-1");
        let here = offered(&mut app);
        assert!(here.iter().any(|l| l == "Reply") && here.iter().any(|l| l == "Forward"));
        assert!(!here.iter().any(|l| l == "Send"), "nothing to send while reading");

        let mut app = inbox(3);
        let here = offered(&mut app);
        assert!(here.iter().any(|l| l == "Show unread only"));
        assert!(!here.iter().any(|l| l == "Show all mail"), "it already shows all");
        assert!(!here.iter().any(|l| l == "Go to Inbox"), "the inbox is on screen");
        assert!(here.iter().any(|l| l == "Go to Drafts"));
        assert!(!here.iter().any(|l| l == "Actions and keys"), "the palette is open");
    }

    /// Each command the palette offers carries the key it is on, as the
    /// keymap has it; while writing, Back is Escape.
    #[test]
    fn the_palette_shows_each_key() {
        let ctx = eframe::egui::Context::default();
        let mut app = inbox(3);
        let lines = app.palette_lines(&ctx);
        let key = |label: &str| {
            lines.iter().find(|(c, _)| c.label == label).and_then(|(c, _)| c.key.clone())
        };
        assert_eq!(key("New email").as_deref(), Some("Ctrl+N"));
        assert_eq!(key("Settings").as_deref(), Some("Ctrl+,"), "a comma, not Comma");
        assert_eq!(key("Add account"), None, "on no key");
        assert_eq!(app.key_of(&ctx, Command::Palette), "Ctrl+K", "a letter over Ctrl+/ and F1");
        assert_eq!(key("Search this account").as_deref(), Some("Ctrl+F"), "a letter over /");
        assert_eq!(key("Back").as_deref(), Some("Backspace"));
        let mut app = writing();
        assert_eq!(app.key_of(&ctx, Command::Back), "Escape");
    }

    /// The palette's keys open it while a text field has the keyboard, and
    /// only those that type nothing: a letter bound to it stays a letter.
    #[test]
    fn the_palette_opens_by_chords_that_type_nothing() {
        let chord = |text| crate::keymap::parse_chord(text).unwrap();
        assert!(types_nothing(&chord("Ctrl+K")));
        assert!(types_nothing(&chord("F1")));
        assert!(!types_nothing(&chord("/")));
        assert!(!types_nothing(&chord("Shift+/")));
        assert!(!types_nothing(&chord("F")));

        let mut app = writing();
        app.exec(Command::Palette, &eframe::egui::Context::default());
        assert!(app.command_palette.is_open());
        app.exec(Command::Palette, &eframe::egui::Context::default());
        assert!(!app.command_palette.is_open());
    }

    /// Show unread only, from the palette, lists the folder again with only
    /// what is unread, as the toggle does.
    #[test]
    fn unread_only_filters_the_list_on_screen() {
        let mut app = inbox(3);
        app.exec(Command::UnreadOnly, &eframe::egui::Context::default());
        assert!(app.screen.unread);
        assert!(app.screen.mailbox.is_none(), "the folder is listed again");
        app.exec(Command::AllMail, &eframe::egui::Context::default());
        assert!(!app.screen.unread);
    }
}
