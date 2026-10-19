//! Settings: the connected accounts — each its initial in its colour and the
//! name it goes by — then Add account. Picking one opens its settings, beside
//! the list on a wide window.

use eframe::egui::{RichText, ScrollArea, Sense, Ui, Vec2};

use crate::commands::Command;
use crate::route::Route;
use crate::state::{Pane, PostApp};
use crate::ui::sidebar;

impl PostApp {
    /// The accounts, then Add account, drawn into `ui`. `shown` is the account
    /// whose settings are beside the list: it is the one marked. On its own
    /// the list marks the keyboard's stop.
    pub fn accounts_list(&mut self, ui: &mut Ui, out: &mut Vec<Command>, shown: Option<&str>) {
        let mut open: Option<(usize, String)> = None;
        let accounts = self.accounts.clone();
        let connecting = self.connecting;
        // One stop per account and "Add account" on the end; a list that just
        // got shorter brings the keyboard back onto it.
        let stops = self.account_stops();
        // The keyboard's stop is the settings list's own — on top, or the step
        // behind the account open beside it — and outlined only while the
        // keyboard is in the list.
        let keyed_list = self.pane() == Pane::List;
        let step = self.list_step();
        let focus = match self.step_of(step) {
            Some((_, screen)) => {
                screen.account_focus = screen.account_focus.min(stops - 1);
                screen.account_focus
            }
            None => 0,
        };
        let follow = keyed_list && std::mem::take(&mut self.follow_focus);
        let adding = keyed_list && focus == stops - 1;
        let names: Vec<String> = accounts
            .iter()
            .flatten()
            .map(|account| self.name_of(account))
            .collect();

        {
            ui.add_space(6.0);
            match accounts.as_deref() {
                None => self.empty(ui, "Reading the account list…"),
                Some([]) => self.empty(
                    ui,
                    "No accounts connected yet. Add one and its mail appears here.",
                ),
                Some(list) => {
                    ScrollArea::vertical().show(ui, |ui| {
                        for (i, email) in list.iter().enumerate() {
                            // As in a mail list: the account whose settings are
                            // open is filled, the keyboard's stop is outlined —
                            // and only in the list being walked.
                            let opened = shown == Some(email.as_str());
                            let keyed = keyed_list && i == focus;
                            let colour = sidebar::account_colour(i);
                            if account_row(ui, email, &names[i], colour, opened, keyed, keyed && follow) {
                                open = Some((i, email.clone()));
                            }
                        }
                    });
                }
            }

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let add = ui
                    .add_enabled(
                        !connecting,
                        omarchy_kit::Button::new("Add account").icon(crate::ui::icons::glyph("plus")),
                    )
                    .on_hover_text(
                        "Opens Google's own consent screen in your browser. \
                         Post never sees the password.",
                    );
                // The keyboard on the last stop: the same outline as on a row.
                if adding {
                    // Two points out from the button, so two points rounder.
                    ui.painter().rect_stroke(
                        add.rect.expand(2.0),
                        omarchy_kit::Size::Regular.radius() + 2.0,
                        eframe::egui::Stroke::new(1.0_f32, ui.visuals().selection.stroke.color),
                        eframe::egui::StrokeKind::Outside,
                    );
                }
                if add.clicked() {
                    out.push(Command::AddAccount);
                }
                if connecting {
                    ui.spinner();
                    ui.label(RichText::new("Finish the sign-in in your browser…").weak());
                }
            });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);
            self.claude_setting(ui);
        }

        if let Some((i, account)) = open {
            if let Some((_, screen)) = self.step_of(step) {
                screen.account_focus = i;
            }
            self.pane = Pane::Reader;
            // Beside an account's settings, another takes their place.
            let route = Route::Account { account };
            if matches!(self.nav.top(), Route::Account { .. }) {
                self.replace(route);
            } else {
                self.goto(route);
            }
        }
    }
}

/// One account, as a row wide enough to click anywhere on: its initial in its
/// colour, the name it goes by, and its address under a name that is not it.
///
/// `follow` is the keyboard having just landed here: the row asks the list to
/// scroll it into view, which does nothing when it is already there.
///
/// `open` is the account whose settings are beside the list: its row is
/// filled. `focused` is the keyboard's stop: an outline.
fn account_row(
    ui: &mut eframe::egui::Ui,
    email: &str,
    name: &str,
    colour: eframe::egui::Color32,
    open: bool,
    focused: bool,
    follow: bool,
) -> bool {
    let height = 48.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    if follow {
        ui.scroll_to_rect(rect, None);
    }
    omarchy_kit::Item {
        open,
        keyed: focused,
        hovered: resp.hovered(),
        size: omarchy_kit::Size::Regular,
    }
    .paint(ui, rect);
    let at = eframe::egui::Pos2::new(rect.min.x + 24.0, rect.center().y);
    sidebar::avatar(
        ui,
        eframe::egui::Rect::from_center_size(at, Vec2::splat(28.0)),
        name,
        colour,
    );
    let x = rect.min.x + 48.0;
    let strong = ui.visuals().strong_text_color();
    let weak = ui.visuals().weak_text_color();
    if name == email {
        ui.painter().text(
            eframe::egui::Pos2::new(x, rect.center().y),
            eframe::egui::Align2::LEFT_CENTER,
            name,
            eframe::egui::FontId::proportional(14.0),
            strong,
        );
    } else {
        ui.painter().text(
            eframe::egui::Pos2::new(x, rect.center().y - 8.0),
            eframe::egui::Align2::LEFT_CENTER,
            name,
            eframe::egui::FontId::proportional(14.0),
            strong,
        );
        ui.painter().text(
            eframe::egui::Pos2::new(x, rect.center().y + 9.0),
            eframe::egui::Align2::LEFT_CENTER,
            email,
            eframe::egui::FontId::monospace(11.5),
            weak,
        );
    }
    resp.on_hover_text("This account's settings").clicked()
}
