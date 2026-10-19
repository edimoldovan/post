//! The window around the screens: which columns are drawn, what goes in
//! them, and the line at the bottom.
//!
//! A folder is its list beside an empty reader; a message is its folder's list
//! — the step behind it — beside the conversation; settings is the accounts in
//! the list column. Compose and reply take both columns. Under 760 the screen
//! on top is the only column.

use eframe::egui::{CentralPanel, Context, RichText, SidePanel, Ui};

use crate::commands::Command;
use crate::route::Route;
use crate::state::PostApp;

impl PostApp {
    /// Draws the screen on top in the columns the window has room for, and
    /// returns whatever it asked for.
    pub fn view(&mut self, ctx: &Context, frame: &mut eframe::Frame) -> Vec<Command> {
        let beside = self.layout.list;
        match self.nav.top().clone() {
            Route::Settings => {
                let mut out = Vec::new();
                match beside {
                    Some(width) => {
                        list_column(ctx, width, |ui| self.accounts_list(ui, &mut out, None));
                        self.nothing_open(ctx);
                    }
                    None => {
                        CentralPanel::default().show(ctx, |ui| self.accounts_list(ui, &mut out, None));
                    }
                }
                out
            }
            // One account's settings, beside the accounts on a wide window.
            Route::Account { account } => {
                let mut out = Vec::new();
                if let Some(width) = beside {
                    list_column(ctx, width, |ui| self.accounts_list(ui, &mut out, Some(&account)));
                }
                out.extend(self.account_view(ctx, &account));
                out
            }
            Route::Folder { account, .. } => {
                let step = self.nav.key();
                match beside {
                    Some(width) => {
                        list_column(ctx, width, |ui| self.list(ui, &account, step));
                        self.nothing_open(ctx);
                    }
                    None => {
                        CentralPanel::default().show(ctx, |ui| self.list(ui, &account, step));
                    }
                }
                Vec::new()
            }
            Route::Message { account, id, .. } => {
                // The list beside a message is its folder's: the step behind.
                if let Some(width) = beside
                    && let Some((step, Route::Folder { .. }, _)) = self.nav.below_mut()
                {
                    list_column(ctx, width, |ui| self.list(ui, &account, step));
                }
                self.detail_view(ctx, frame, &account, &id)
            }
            Route::Reply { account, .. }
            | Route::New { account, .. }
            | Route::Draft { account, .. }
            | Route::Forward { account, .. } => self.compose_view(ctx, &account),
        }
    }

    /// The reader with nothing in it, beside a list.
    fn nothing_open(&self, ctx: &Context) {
        CentralPanel::default().show(ctx, |ui| self.empty(ui, "Nothing open."));
    }

    /// One line: what is happening, or what went wrong, and where we are.
    pub fn status_bar(&mut self, ctx: &Context) {
        let name = self.account().map(|account| self.name_of(account)).unwrap_or_default();
        let where_ = match self.nav.top() {
            Route::Settings => "Accounts".to_string(),
            Route::Folder { .. } => match &self.screen.mailbox {
                Some(mailbox) => format!("{name} — {} unread", mailbox.unread()),
                None => name,
            },
            Route::Message { .. } => name,
            Route::Reply { .. } => format!("{name} — replying"),
            Route::New { .. } => format!("{name} — new message"),
            Route::Draft { .. } => format!("{name} — draft"),
            Route::Forward { .. } => format!("{name} — forwarding"),
            Route::Account { .. } => format!("{name} — settings"),
        };
        let status = self.status.clone();
        let connecting = self.connecting;
        let mut reconnect = false;
        let mut line = omarchy_kit::StatusLine::new("post-status").place(where_).message(status);
        // A dead Google grant is repaired from right here: the same consent
        // flow that connected the account replaces its tokens — no terminal.
        if self.reconnect {
            line = line.after(|ui| {
                let button = omarchy_kit::Button::new("Reconnect").danger().small();
                reconnect = ui.add_enabled(!connecting, button).clicked();
            });
        }
        line.show(ctx);
        if reconnect {
            self.start(crate::client::Job::AddAccount);
        }
    }

    /// A sentence for a view with nothing in it yet — never a blank rectangle.
    pub fn empty(&self, ui: &mut Ui, sentence: &str) {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(sentence).weak());
        });
    }
}

/// The list column, beside the reader: a fixed width, the designs'.
fn list_column(ctx: &Context, width: f32, show: impl FnOnce(&mut Ui)) {
    SidePanel::left("post-list")
        .exact_width(width)
        .resizable(false)
        .show(ctx, show);
}
