//! One account's settings: the name it goes by, and Remove.
//!
//! The name is the address until the reader gives it another; it shows
//! everywhere the address did. Remove asks first, on the screen itself, and
//! then takes the account off this computer — its sign-in, and the mail Post
//! keeps for it. Gmail itself is untouched.

use eframe::egui::{CentralPanel, Context, RichText, Sense, Vec2};
use omarchy_kit::{Button, TextField};

use crate::client::Job;
use crate::commands::Command;
use crate::state::PostApp;
use crate::ui::{icons, sidebar};

impl PostApp {
    pub fn account_view(&mut self, ctx: &Context, account: &str) -> Vec<Command> {
        let mut out = Vec::new();
        // The back arrow is for a screen with no list beside it.
        let back = self.layout.list.is_none();
        let name = self.name_of(account);
        let colour = self
            .accounts
            .as_ref()
            .and_then(|list| list.iter().position(|a| a == account))
            .map_or(self.palette.accent, sidebar::account_colour);
        // The field starts with the name it goes by — the address, until it
        // has another.
        let mut edit = self
            .screen
            .name_edit
            .get_or_insert_with(|| name.clone())
            .clone();
        let mut confirm = self.screen.confirm_remove;
        let removing = self.jobs.busy() && confirm;
        let mut rename: Option<String> = None;
        let mut remove = false;

        CentralPanel::default().show(ctx, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if back
                    && ui
                        .add(Button::icon_only(icons::glyph("back")).quiet().tip("Back"))
                        .clicked()
                {
                    out.push(Command::Back);
                }
                let (spot, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
                sidebar::avatar(ui, spot.shrink(2.0), &name, colour);
                ui.vertical(|ui| {
                    ui.label(RichText::new(&name).size(18.0).strong());
                    if name != account {
                        ui.label(RichText::new(account).monospace().weak());
                    }
                });
            });
            ui.add_space(18.0);

            ui.label(RichText::new("Name").weak());
            let field = ui.add(
                TextField::new(&mut edit)
                    .hint(account)
                    .width(ui.available_width().min(360.0)),
            );
            ui.label(
                RichText::new("What this account goes by in Post. Empty, it goes by its address.")
                    .weak()
                    .size(12.0),
            );
            // Enter or leaving the field keeps the name.
            if field.lost_focus() {
                let wanted = match edit.trim() {
                    same if same == account => "",
                    wanted => wanted,
                };
                let current = if name == account { "" } else { name.as_str() };
                if wanted != current {
                    rename = Some(wanted.to_string());
                }
            }

            ui.add_space(22.0);
            ui.separator();
            ui.add_space(12.0);
            if !confirm {
                if ui.add(Button::new("Remove from Post…")).clicked() {
                    confirm = true;
                }
                return;
            }
            ui.label(RichText::new(format!("Remove {account} from Post?")).strong());
            ui.label(
                RichText::new(
                    "Its sign-in, and the mail Post keeps for it on this computer, go. Gmail \
                     itself is untouched, and the account can be connected again.",
                )
                .weak(),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = if removing { "Removing…" } else { "Remove" };
                if ui.add_enabled(!removing, Button::new(label).danger()).clicked() {
                    remove = true;
                }
                if ui.add_enabled(!removing, Button::new("Cancel")).clicked() {
                    confirm = false;
                }
            });
        });

        self.screen.name_edit = Some(edit);
        self.screen.confirm_remove = confirm;
        if let Some(name) = rename {
            self.start(Job::Rename {
                account: account.to_string(),
                name,
            });
        }
        if remove {
            self.start(Job::Remove {
                account: account.to_string(),
            });
        }
        out
    }
}
