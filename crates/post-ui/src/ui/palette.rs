//! The command palette: every action there is where the window is, found by
//! typing — the commands, each with its key, and what is on screen to act on:
//! the folders, the accounts, who it is from, the attachments.

use eframe::egui::{Context, Key, KeyboardShortcut, Modifiers};
use omarchy_kit::Choice;

use crate::commands::Command;
use crate::route::Route;
use crate::state::{Pane, PostApp};
use crate::ui::detail::Pick;

/// What one line of the palette does.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// A command, as its key runs it.
    Run(Command),
    /// A folder, an account's inbox, an account's settings.
    Go(Route),
    /// Another account to send what is being written from.
    From(String),
    /// One of the files going with what is being written, taken off.
    TakeOff(usize),
    /// One of the open message's attachments, opened or saved.
    Attachment { id: String, name: String, pick: Pick },
}

impl PostApp {
    /// The palette while it is open, and what is chosen in it done.
    pub fn palette_view(&mut self, ctx: &Context) {
        if !self.command_palette.is_open() {
            return;
        }
        let (choices, actions): (Vec<Choice>, Vec<Action>) = self.palette_lines(ctx).into_iter().unzip();
        let key = self.key_of(ctx, Command::Palette);
        if let Some(action) = self.command_palette.show(ctx, &key, &choices).and_then(|i| actions.into_iter().nth(i)) {
            self.act(action, ctx);
        }
    }

    /// The key `cmd` is on, as the window shows it; empty when it is on none.
    /// Of several, a letter's chord reads best — Ctrl+K over Ctrl+/ — and a
    /// function key or a shifted one worst.
    pub fn key_of(&self, ctx: &Context, cmd: Command) -> String {
        // While writing, Escape goes back: the editor has the other keys.
        if cmd == Command::Back && self.nav.top().writing() {
            return spell(ctx, &KeyboardShortcut::new(Modifiers::NONE, Key::Escape));
        }
        let worse = |shortcut: &KeyboardShortcut| {
            let name = shortcut.logical_key.name();
            let function = name.len() > 1 && name.starts_with('F') && name[1..].chars().all(|c| c.is_ascii_digit());
            let letter = name.len() == 1 && name.chars().all(|c| c.is_ascii_alphabetic());
            (function || shortcut.modifiers.shift, !letter)
        };
        self.keymap
            .bindings
            .iter()
            .filter(|(_, bound)| *bound == cmd)
            .map(|(shortcut, _)| shortcut)
            .min_by_key(|shortcut| worse(shortcut))
            .map(|shortcut| spell(ctx, shortcut))
            .unwrap_or_default()
    }

    /// Every line the palette offers where the window is: the commands that
    /// do something here, then what is on screen to act on, then where to go.
    pub fn palette_lines(&mut self, ctx: &Context) -> Vec<(Choice, Action)> {
        let top = self.nav.top().clone();
        let mail_list = matches!(top, Route::Folder { .. })
            || (matches!(top, Route::Message { .. }) && self.layout.list.is_some());
        let unread = match (mail_list, self.list_step()) {
            (true, Some(step)) => self.step_of(Some(step)).map(|(_, screen)| screen.unread),
            _ => None,
        };
        let mut lines: Vec<(Choice, Action)> = Command::ALL
            .into_iter()
            .filter(|&cmd| self.applies(cmd, mail_list, unread))
            .map(|cmd| {
                let key = Some(self.key_of(ctx, cmd)).filter(|key| !key.is_empty());
                (Choice { label: cmd.label().to_string(), key }, Action::Run(cmd))
            })
            .collect();
        let line = |label: String, action: Action| (Choice { label, key: None }, action);
        let accounts = self.accounts.clone().unwrap_or_default();
        let current = self.account().map(str::to_owned);

        // Who it is from, where that can change: a new email, with another
        // account to send it from.
        let (saving, sending) = (self.screen.saving, self.screen.sending);
        if matches!(top, Route::New { .. } | Route::Draft { .. }) && !saving && !sending {
            let from = self.writing_account().or(current.clone());
            for account in accounts.iter().filter(|a| Some(*a) != from.as_ref()) {
                lines.push(line(format!("Send from {}", self.name_of(account)), Action::From(account.clone())));
            }
        }
        if top.writing() {
            for (i, path) in self.screen.attach.iter().enumerate() {
                let name = std::path::Path::new(path).file_name().map_or(path.clone(), |n| n.to_string_lossy().into_owned());
                lines.push(line(format!("Take off {name}"), Action::TakeOff(i)));
            }
        }
        if let Some(message) = self.screen.conversation.as_ref().and_then(|c| c.opened()) {
            for file in &message.attachments {
                for (label, pick) in [
                    (format!("Open {}", file.name), Pick::Open),
                    (format!("Save {}", file.name), Pick::Save),
                    (format!("Save {} as…", file.name), Pick::SaveAs),
                ] {
                    let (id, name) = (message.id.clone(), file.name.clone());
                    lines.push(line(label, Action::Attachment { id, name, pick }));
                }
            }
        }

        // Where to go: the account on screen's folders, the other accounts,
        // and every account's settings.
        if let Some(account) = &current {
            for label in self.folders.get(account).into_iter().flatten() {
                let route = Route::Folder { account: account.clone(), folder: label.id.clone() };
                if route != top {
                    lines.push(line(format!("Go to {}", label.name), Action::Go(route)));
                }
            }
        }
        for account in accounts.iter().filter(|a| Some(*a) != current.as_ref()) {
            lines.push(line(format!("Go to {}", self.name_of(account)), Action::Go(Route::inbox(account.as_str()))));
        }
        for account in &accounts {
            let route = Route::Account { account: account.clone() };
            if route != top {
                lines.push(line(format!("Settings for {}", self.name_of(account)), Action::Go(route)));
            }
        }
        lines
    }

    /// Whether `cmd` does something where the window is. `mail_list` is
    /// whether a list of mail is on screen, and `unread` whether it shows only
    /// what is unread.
    fn applies(&self, cmd: Command, mail_list: bool, unread: Option<bool>) -> bool {
        let top = self.nav.top();
        let writing = top.writing();
        let reading = top.message().is_some();
        match cmd {
            Command::Reply | Command::Forward | Command::MarkUnread => reading,
            Command::Send | Command::Attach | Command::Discard => writing && !self.screen.sending,
            Command::Copies => writing && !self.screen.copies,
            Command::Delete => reading || mail_list,
            Command::Search => mail_list,
            Command::AllMail => unread == Some(true),
            Command::UnreadOnly => unread == Some(false),
            Command::Refresh => !matches!(
                top,
                Route::New { .. } | Route::Draft { .. } | Route::Forward { .. } | Route::Account { .. }
            ),
            Command::Accounts => !matches!(top, Route::Settings),
            Command::AddAccount => !self.connecting,
            Command::New
            | Command::Open
            | Command::Next
            | Command::Previous
            | Command::PageNext
            | Command::PagePrevious
            | Command::Last
            | Command::Left
            | Command::Right => !writing,
            // The palette is open, and what is copied is what the mouse
            // selected, from the menu the selection has.
            Command::Palette | Command::Copy => false,
            Command::Back | Command::Quit => true,
        }
    }

    /// Does what a line of the palette says.
    fn act(&mut self, action: Action, ctx: &Context) {
        match action {
            Action::Run(cmd) => self.exec(cmd, ctx),
            // As the sidebar and the settings list go there.
            Action::Go(route) => {
                let settings = matches!(route, Route::Account { .. });
                if settings && matches!(self.nav.top(), Route::Account { .. }) {
                    self.replace(route);
                } else {
                    self.goto(route);
                }
                self.pane = if settings { Pane::Reader } else { Pane::List };
            }
            Action::From(account) => self.change_from(account),
            Action::TakeOff(i) => {
                if i < self.screen.attach.len() {
                    self.screen.attach.remove(i);
                }
            }
            Action::Attachment { id, name, pick } => {
                if let Some(account) = self.account().map(str::to_owned) {
                    self.attachment(ctx, &account, id, name, pick);
                }
            }
        }
    }
}

/// A chord as egui spells it, but with "/" and "," for the slash and the
/// comma it calls Slash and Comma.
fn spell(ctx: &Context, shortcut: &KeyboardShortcut) -> String {
    let text = ctx.format_shortcut(shortcut);
    match shortcut.logical_key {
        Key::Slash => text.replace("Slash", "/"),
        Key::Comma => text.replace("Comma", ","),
        _ => text,
    }
}
