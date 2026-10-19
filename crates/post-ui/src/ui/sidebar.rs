//! The sidebar, and the rail it narrows to under 1000.
//!
//! Post's mark, Compose, then the accounts — each its own Post, with an
//! initial in its colour and its inbox's unread count — then the folders of
//! the account on screen, and the settings gear at the bottom left. The rail is
//! the same thing as icons.

use eframe::egui::{
    Align, Align2, Color32, Context, FontId, Layout, Painter, Pos2, Rect, RichText, ScrollArea,
    Sense, Shape, SidePanel, Stroke, Ui, Vec2,
};

use omarchy_kit::{Button, Item, Size};

use crate::client::Label;
use crate::commands::Command;
use crate::route::{INBOX, Route};
use crate::state::{Pane, PostApp};
use crate::ui::detail::tip;
use crate::ui::icons;

/// Account colours: a fixed set, given out in account order.
const COLOURS: [Color32; 6] = [
    Color32::from_rgb(0x3d, 0x8f, 0x6f),
    Color32::from_rgb(0x55, 0x63, 0xb8),
    Color32::from_rgb(0xc8, 0x6b, 0x34),
    Color32::from_rgb(0x9a, 0x5c, 0xb4),
    Color32::from_rgb(0x2f, 0x8c, 0xa8),
    Color32::from_rgb(0xb8, 0x4a, 0x5a),
];

/// The colour of the account at `index` in the account list.
pub fn account_colour(index: usize) -> Color32 {
    COLOURS[index % COLOURS.len()]
}

/// An account's initial: the first letter of its name, which is its address
/// until it is renamed.
pub fn initial(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default()
}

/// Text that reads on `fill`: white on a dark colour, near-black on a light one.
pub fn on(fill: Color32) -> Color32 {
    let light = 0.299 * fill.r() as f32 + 0.587 * fill.g() as f32 + 0.114 * fill.b() as f32;
    if light > 160.0 {
        Color32::from_gray(20)
    } else {
        Color32::WHITE
    }
}

/// An account's initial on its colour, a rounded square in the small corner.
pub fn avatar(ui: &Ui, rect: Rect, name: &str, colour: Color32) {
    ui.painter().rect_filled(rect, Size::Small.radius(), colour);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        initial(name),
        FontId::proportional(rect.height() * 0.5),
        on(colour),
    );
}

impl PostApp {
    /// Draws the sidebar, or the rail, and returns whatever it asked for.
    pub fn sidebar(&mut self, ctx: &Context) -> Vec<Command> {
        let mut out = Vec::new();
        let rail = self.layout.rail();
        let accounts = self.accounts.clone().unwrap_or_default();
        let current = self.account().map(str::to_owned);
        // The folders are the account on screen's, with the one on screen
        // marked; a search is no folder of the sidebar's.
        let folders = current
            .as_ref()
            .and_then(|account| self.folders.get(account))
            .cloned()
            .unwrap_or_default();
        let folder = self.nav.top().folder().map(str::to_owned);
        let inbox_unread = |account: &str| {
            self.folders
                .get(account)
                .and_then(|labels| labels.iter().find(|l| l.id == INBOX))
                .map_or(0, |inbox| inbox.unread)
        };
        let counts: Vec<u64> = accounts.iter().map(|a| inbox_unread(a)).collect();
        let on_settings = matches!(self.nav.top(), Route::Settings | Route::Account { .. });
        // The keyboard's stop, outlined while the keyboard is in the sidebar.
        let keyed = (self.pane() == Pane::Sidebar).then_some(self.side_focus);
        let follow = std::mem::take(&mut self.side_follow);
        let names: Vec<String> = accounts.iter().map(|a| self.name_of(a)).collect();
        let mut picked: Option<Route> = None;
        // What the tools at the edges do, and the keys they are also on.
        let new_tip = tip("A new email", &self.key_of(ctx, Command::New));
        let gear_tip = tip("Settings", &self.key_of(ctx, Command::Accounts));
        let keys_tip = tip("Actions and keys", &self.key_of(ctx, Command::Palette));
        let palette_open = self.command_palette.is_open();

        SidePanel::left("post-sidebar")
            .exact_width(self.layout.side)
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_space(14.0);
                mark(ui, rail);
                ui.add_space(14.0);
                if compose_button(ui, rail, &new_tip) {
                    out.push(Command::New);
                }
                ui.add_space(14.0);
                // Accounts and folders scroll; the gear stays at the bottom.
                let room = ui.available_height() - 56.0;
                ScrollArea::vertical()
                    .id_salt("post-sidebar")
                    .max_height(room.max(0.0))
                    .show(ui, |ui| {
                        section(ui, rail, "ACCOUNTS");
                        for (i, account) in accounts.iter().enumerate() {
                            let here = current.as_deref() == Some(account.as_str());
                            let stop = Stop { here, keyed: keyed == Some(i), follow };
                            if account_entry(ui, &names[i], account_colour(i), counts[i], stop, rail) {
                                picked = Some(Route::inbox(account.as_str()));
                            }
                        }
                        if let (Some(account), false) = (&current, folders.is_empty()) {
                            ui.add_space(10.0);
                            section(ui, rail, "FOLDERS");
                            for (j, label) in folders.iter().enumerate() {
                                let here = folder.as_deref() == Some(label.id.as_str());
                                let stop = Stop {
                                    here,
                                    keyed: keyed == Some(accounts.len() + j),
                                    follow,
                                };
                                if folder_entry(ui, label, stop, rail) {
                                    picked = Some(Route::Folder {
                                        account: account.clone(),
                                        folder: label.id.clone(),
                                    });
                                }
                            }
                        }
                    });
                // The gear, at the bottom left, and the palette beside it —
                // on the rail, over it.
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.add_space(12.0);
                    let (settings, palette) = if rail {
                        ui.vertical_centered(|ui| {
                            let palette = keys(ui, palette_open, &keys_tip);
                            (gear(ui, on_settings, &gear_tip), palette)
                        })
                        .inner
                    } else {
                        ui.horizontal(|ui| {
                            let settings = gear(ui, on_settings, &gear_tip);
                            (settings, keys(ui, palette_open, &keys_tip))
                        })
                        .inner
                    };
                    if settings {
                        out.push(Command::Accounts);
                    }
                    if palette {
                        out.push(Command::Palette);
                    }
                });
            });

        if let Some(route) = picked {
            self.goto(route);
            self.pane = Pane::List;
        }
        out
    }
}

/// How one stop of the sidebar is marked: `here` is what is on screen,
/// filled; `keyed` is the keyboard's stop, outlined — and brought into view
/// when the keyboard has just moved (`follow`).
#[derive(Clone, Copy)]
struct Stop {
    here: bool,
    keyed: bool,
    follow: bool,
}

impl Stop {
    /// Fills and outlines `cell` as this stop is marked: the kit's marks.
    fn paint(self, ui: &Ui, cell: Rect, hovered: bool) {
        Item {
            open: self.here,
            keyed: self.keyed,
            hovered,
            size: Size::Regular,
        }
        .paint(ui, cell);
    }
}

/// A section's caption on the sidebar; on the rail, a line.
fn section(ui: &mut Ui, rail: bool, caption: &str) {
    if rail {
        ui.separator();
    } else {
        ui.label(RichText::new(caption).size(11.0).weak());
    }
    ui.add_space(4.0);
}

/// The icon a folder is drawn with: Gmail's own for the system folders, a
/// label for the account's own.
fn folder_icon(label: &Label) -> &'static str {
    match label.id.as_str() {
        "INBOX" => "inbox",
        "STARRED" => "star",
        "IMPORTANT" => "important",
        "SENT" => "send",
        "DRAFT" => "draft",
        "SPAM" => "spam",
        "TRASH" => "trash",
        _ => "label",
    }
}

/// One folder: its icon, its name — a nested label's last part, indented
/// under its parent — and how many of its conversations are unread, or, for
/// Drafts, how many drafts there are. On the rail, the icon alone. Returns
/// true when it was clicked.
fn folder_entry(ui: &mut Ui, label: &Label, stop: Stop, rail: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), if rail { 44.0 } else { 34.0 }),
        Sense::click(),
    );
    if stop.keyed && stop.follow {
        ui.scroll_to_rect(rect, None);
    }
    let cell = if rail {
        Rect::from_center_size(rect.center(), Vec2::splat(40.0))
    } else {
        rect
    };
    stop.paint(ui, cell, resp.hovered());
    let here = stop.here;
    let ink = if here {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().text_color()
    };
    // Drafts counts its drafts, none of them ever unread; every other folder
    // counts what is unread in it.
    let (count, tip) = match (label.id.as_str(), label.unread, label.total) {
        ("DRAFT", _, 0) => (0, label.name.clone()),
        ("DRAFT", _, n) => (n, format!("{} — {n}", label.name)),
        (_, 0, _) => (0, label.name.clone()),
        (_, n, _) => (n, format!("{} — {n} unread", label.name)),
    };
    if rail {
        icons::draw(
            ui.painter(),
            Rect::from_center_size(rect.center(), Vec2::splat(19.0)),
            folder_icon(label),
            ink,
        );
        return resp.on_hover_text(tip).clicked();
    }
    let depth = label.name.matches('/').count() as f32;
    let x = rect.min.x + 10.0 + depth * 14.0;
    icons::draw(
        ui.painter(),
        Rect::from_center_size(Pos2::new(x + 9.0, rect.center().y), Vec2::splat(17.0)),
        folder_icon(label),
        ink,
    );
    let right = match count {
        0 => rect.max.x - 6.0,
        n => {
            ui.painter()
                .text(
                    Pos2::new(rect.max.x - 8.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    n.to_string(),
                    FontId::monospace(12.0),
                    ui.visuals().weak_text_color(),
                )
                .min
                .x
                - 8.0
        }
    };
    let name = label.name.rsplit('/').next().unwrap_or(&label.name);
    ui.painter()
        .with_clip_rect(Rect::from_min_max(
            Pos2::new(x + 26.0, rect.min.y),
            Pos2::new(right, rect.max.y),
        ))
        .text(
            Pos2::new(x + 26.0, rect.center().y),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(14.0),
            ink,
        );
    resp.on_hover_text(tip).clicked()
}

/// Post's mark: its logo, and the name beside it.
fn mark(ui: &mut Ui, rail: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::hover());
    let x = if rail { rect.center().x - 14.0 } else { rect.min.x + 2.0 };
    let square = Rect::from_min_size(Pos2::new(x, rect.center().y - 14.0), Vec2::splat(28.0));
    logo(ui.painter(), square);
    if !rail {
        ui.painter().text(
            Pos2::new(square.max.x + 12.0, rect.center().y),
            Align2::LEFT_CENTER,
            "Post",
            FontId::proportional(18.0),
            ui.visuals().strong_text_color(),
        );
    }
}

/// Post's logo, as `packaging/post.svg` draws it on its 64-point
/// square: a white envelope on Post's green, its flap and folds in a paler
/// green. Its own colours, whatever the theme.
fn logo(painter: &Painter, rect: Rect) {
    let scale = rect.width() / 64.0;
    let at = |x: f32, y: f32| rect.min + Vec2::new(x, y) * scale;
    let fold = Color32::from_rgb(0x9e, 0xc7, 0xad);
    painter.rect_filled(rect, 12.0 * scale, Color32::from_rgb(0x1e, 0x71, 0x45));
    painter.rect_filled(Rect::from_min_max(at(8.0, 18.0), at(56.0, 46.0)), 2.0 * scale, Color32::WHITE);
    painter.add(Shape::line(
        vec![at(8.0, 19.0), at(32.0, 38.0), at(56.0, 19.0)],
        Stroke::new(2.5 * scale, fold),
    ));
    painter.line_segment([at(8.0, 45.0), at(23.0, 33.0)], Stroke::new(1.5 * scale, fold));
    painter.line_segment([at(56.0, 45.0), at(41.0, 33.0)], Stroke::new(1.5 * scale, fold));
}

/// Compose: the one primary button, as wide as the sidebar — on the rail,
/// its icon alone.
fn compose_button(ui: &mut Ui, rail: bool, tip: &str) -> bool {
    let button = if rail {
        Button::icon_only(icons::glyph("compose")).primary().large()
    } else {
        Button::new("Compose")
            .icon(icons::glyph("compose"))
            .primary()
            .large()
            .width(ui.available_width())
    };
    ui.vertical_centered(|ui| ui.add(button))
        .inner
        .on_hover_text(tip)
        .clicked()
}

/// One account: its initial in its colour and, on the sidebar, its name. The
/// account on screen is marked. Returns true when it was clicked.
fn account_entry(
    ui: &mut Ui,
    account: &str,
    colour: Color32,
    unread: u64,
    stop: Stop,
    rail: bool,
) -> bool {
    let height = if rail { 48.0 } else { 44.0 };
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    if stop.keyed && stop.follow {
        ui.scroll_to_rect(rect, None);
    }
    let cell = if rail {
        Rect::from_center_size(rect.center(), Vec2::splat(44.0))
    } else {
        rect
    };
    stop.paint(ui, cell, resp.hovered());
    let here = stop.here;
    let size = if rail { 30.0 } else { 26.0 };
    let at = if rail {
        rect.center()
    } else {
        Pos2::new(rect.min.x + 8.0 + size / 2.0, rect.center().y)
    };
    avatar(ui, Rect::from_center_size(at, Vec2::splat(size)), account, colour);
    if !rail {
        // The inbox's unread conversations, at the end of the line.
        let right = match unread {
            0 => rect.max.x - 6.0,
            n => {
                ui.painter()
                    .text(
                        Pos2::new(rect.max.x - 8.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        n.to_string(),
                        FontId::monospace(12.0),
                        ui.visuals().weak_text_color(),
                    )
                    .min
                    .x
                    - 8.0
            }
        };
        let name = Rect::from_min_max(
            Pos2::new(at.x + size / 2.0 + 10.0, rect.min.y),
            Pos2::new(right, rect.max.y),
        );
        ui.painter().with_clip_rect(name).text(
            Pos2::new(name.min.x, rect.center().y),
            Align2::LEFT_CENTER,
            account,
            FontId::proportional(14.0),
            if here {
                ui.visuals().strong_text_color()
            } else {
                ui.visuals().text_color()
            },
        );
    }
    resp.on_hover_text(format!("{account}'s mail")).clicked()
}

/// The settings gear, a quiet button: marked while settings is on screen.
fn gear(ui: &mut Ui, here: bool, tip: &str) -> bool {
    let button = Button::icon_only(icons::glyph("gear")).quiet().selected(here);
    ui.add(button).on_hover_text(tip).clicked()
}

/// The palette's button: every action and the key it is on, marked while
/// the palette is open.
fn keys(ui: &mut Ui, open: bool, tip: &str) -> bool {
    let button = Button::icon_only(icons::glyph("keyboard")).quiet().selected(open);
    ui.add(button).on_hover_text(tip).clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An account's initial is the first letter of its name, and colours go
    /// round the set in account order.
    #[test]
    fn an_account_is_an_initial_in_its_colour() {
        assert_eq!(initial("ed@acme.com"), "E");
        assert_eq!(initial("_billing@acme.com"), "B");
        assert_eq!(initial(""), "");
        assert_eq!(account_colour(0), account_colour(COLOURS.len()));
        assert_ne!(account_colour(0), account_colour(1));
        assert_eq!(on(Color32::from_rgb(0xf0, 0xe0, 0x60)), Color32::from_gray(20));
        assert_eq!(on(Color32::from_rgb(0x3d, 0x8f, 0x6f)), Color32::WHITE);
    }
}
