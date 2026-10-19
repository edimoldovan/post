//! The conversation: every message on it, oldest at the top, newest at the
//! bottom, and the open one's body directly under its own line.
//!
//! A message is one line — who wrote it and when — and clicking it (or Enter on
//! it) opens it in place: the body appears between that line and the next, as
//! tall as it is, and the conversation scrolls over it like anything else. Only
//! one is open at a time.

use eframe::egui::{
    Align, Align2, CentralPanel, Color32, Context, FontId, Layout, Popup, Pos2, Rect, RichText,
    ScrollArea, Sense, Ui, Vec2,
};

use omarchy_kit::{Button, Item, Size};

use crate::client::Message;
use crate::commands::Command;
use crate::state::PostApp;
use crate::ui::{icons, sidebar};

/// One message's line in the conversation.
const LINE_H: f32 = 30.0;
const LINE_X: f32 = 8.0;
const LINE_DATE_W: f32 = 116.0;

/// How far a trackpad or wheel moves the conversation, against egui's own
/// scroll: as far as it moved the WebKit body. GTK reads ten Wayland axis units
/// as one scroll step and WebKit moved 40 px per step; egui moves one point
/// per unit.
const WHEEL: f32 = 4.0;

impl PostApp {
    pub fn detail_view(
        &mut self,
        ctx: &Context,
        _frame: &mut eframe::Frame,
        account: &str,
        id: &str,
    ) -> Vec<Command> {
        let mut out = Vec::new();
        // The back arrow is for a reader with no list beside it; the labels
        // for one with room for them.
        let back = self.layout.list.is_none();
        let labels = !self.layout.rail();
        let hint = |cmd| {
            self.keymap
                .shortcut_for(cmd)
                .map(|shortcut| ctx.format_shortcut(&shortcut))
                .unwrap_or_default()
        };
        let hints = Hints {
            back: hint(Command::Back),
            reply: hint(Command::Reply),
            forward: hint(Command::Forward),
            delete: hint(Command::Delete),
            unread: hint(Command::MarkUnread),
        };
        let colour = self
            .accounts
            .as_ref()
            .and_then(|list| list.iter().position(|a| a == account))
            .map_or(self.palette.accent, sidebar::account_colour);
        let account_name = self.name_of(account);
        if self.screen.conversation.is_none() {
            CentralPanel::default().show(ctx, |ui| {
                toolbar(ui, back, labels, &hints, &mut out);
                self.empty(ui, "Opening the conversation…");
            });
            return out;
        }

        // egui hands the panel a closure, and the closure wants all of `self`.
        // The two big fields step out for the duration and go straight back,
        // rather than being copied every frame: a mail body is measured in
        // hundreds of kilobytes.
        let conversation = self.screen.conversation.take().expect("a conversation");
        let page = self.screen.body.take();
        // Whatever the page keys asked for since the last frame: it scrolls the
        // conversation, body and all.
        let scroll = std::mem::take(&mut self.body_scroll);
        let follow = std::mem::take(&mut self.follow_focus);
        let open = conversation.opened().map(|message| message.id.clone());
        let key = format!("{account}/{}", open.as_deref().unwrap_or(id));
        let (background, accent) = (self.palette.bg, self.palette.accent);
        // Each step of the history scrolls on its own: Back finds the
        // conversation where it was left.
        let step = self.nav.key();
        let mut tapped: Option<usize> = None;
        let mut taken: Option<(String, String, Pick)> = None;

        CentralPanel::default().show(ctx, |ui| {
            toolbar(ui, back, labels, &hints, &mut out);
            ui.add_space(14.0);
            ui.label(
                RichText::new(if conversation.subject.is_empty() {
                    "(no subject)"
                } else {
                    &conversation.subject
                })
                .size(22.0)
                .strong(),
            );
            if conversation.messages.len() > 1 {
                ui.label(
                    RichText::new(format!("{} messages", conversation.messages.len()))
                        .size(12.0)
                        .weak(),
                );
            }
            ui.add_space(4.0);
            ui.separator();

            // The conversation itself: one line per message, oldest first, and
            // the open one's body between its line and the next.
            if conversation.opened().is_none() {
                self.html.close();
            }
            ScrollArea::vertical()
                .id_salt(("post-conversation", step))
                .wheel_scroll_multiplier(Vec2::new(1.0, WHEEL))
                .show(ui, |ui| {
                    // What the page keys asked for, which here moves the
                    // conversation: egui moves the content rather than the
                    // window onto it, so it goes in negated.
                    if scroll != 0.0 {
                        ui.scroll_with_delta(Vec2::new(0.0, -scroll));
                    }
                    for (i, message) in conversation.messages.iter().enumerate() {
                        let on = conversation.open == Some(i);
                        let here = conversation.focus == i;
                        // The open message's header is the designs': initial, name
                        // and address, the account it came to, when. The others
                        // are a line each.
                        let clicked = if on {
                            sender_block(ui, message, &account_name, colour, here && follow)
                        } else {
                            attribution(ui, message, on, here, here && follow)
                        };
                        if clicked {
                            tapped = Some(i);
                        }
                        if !on {
                            continue;
                        }
                        match &page {
                            Some(page) => {
                                self.html.show(ui, &key, &page.html, background, accent)
                            }
                            None => {
                                self.html.close();
                                text_body(ui, message);
                            }
                        }
                        if !message.attachments.is_empty() {
                            ui.add_space(6.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                                for file in &message.attachments {
                                    if let Some(pick) = attachment_card(ui, file) {
                                        taken = Some((message.id.clone(), file.name.clone(), pick));
                                    }
                                }
                            });
                            ui.add_space(6.0);
                        }
                    }
                });
            if conversation.opened().is_none() {
                ui.add_space(6.0);
                ui.label(RichText::new("Nothing is open — press Enter on a message.").weak());
            }
        });
        self.screen.conversation = Some(conversation);
        self.screen.body = page;
        if let Some((id, name, pick)) = taken {
            self.attachment(ctx, account, id, name, pick);
        }
        // A line tapped is the keyboard landing there and Enter on it: the
        // mouse and the key press are the same thing happening.
        if let Some(i) = tapped {
            if let Some(conversation) = &mut self.screen.conversation {
                conversation.focus = i;
            }
            self.toggle_message();
        }
        out
    }
}

impl PostApp {
    /// One of a message's attachments: opened, saved to Downloads, or saved
    /// where the file picker says.
    pub fn attachment(&mut self, ctx: &Context, account: &str, id: String, name: String, pick: Pick) {
        let account = account.to_string();
        match pick {
            Pick::Open | Pick::Save => {
                let open = pick == Pick::Open;
                self.start(crate::client::Job::Attachment { account, id, name, path: None, open });
            }
            Pick::SaveAs => {
                // Its own thread: see `PostApp::picked_tx`.
                let (picked, ctx) = (self.picked_tx.clone(), ctx.clone());
                std::thread::spawn(move || {
                    if let Some(path) = rfd::FileDialog::new().set_file_name(name.as_str()).save_file() {
                        let path = path.to_string_lossy().into_owned();
                        let _ = picked.send(crate::state::Picked::SaveAs { account, id, name, path });
                        ctx.request_repaint();
                    }
                });
            }
        }
    }
}

/// The key each button on the bar is also on, as the keymap has it.
struct Hints {
    back: String,
    reply: String,
    forward: String,
    delete: String,
    unread: String,
}

/// The reader's bar: the back arrow where the list is not beside it, Reply,
/// Forward, and at the far end Delete, with the rest behind ⋯. Quiet buttons,
/// their words and keys where there is room.
fn toolbar(ui: &mut Ui, back: bool, labels: bool, hints: &Hints, out: &mut Vec<Command>) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if back && ui.add(tool("back", "Back", "", false)).on_hover_text(tip("Back", &hints.back)).clicked() {
            out.push(Command::Back);
        }
        if ui
            .add(tool("reply", "Reply", &hints.reply, labels))
            .on_hover_text(tip("Reply", &hints.reply))
            .clicked()
        {
            out.push(Command::Reply);
        }
        if ui
            .add(tool("forward", "Forward", &hints.forward, labels))
            .on_hover_text(tip("Forward", &hints.forward))
            .clicked()
        {
            out.push(Command::Forward);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let more = ui.add(tool("more", "More", "", false)).on_hover_text("More");
            Popup::menu(&more).show(|ui| {
                if ui
                    .add(Button::new("Mark unread").quiet().hint(&hints.unread))
                    .clicked()
                {
                    out.push(Command::MarkUnread);
                    ui.close();
                }
            });
            if ui
                .add(tool("trash", "Delete", &hints.delete, labels))
                .on_hover_text(tip("Delete", &hints.delete))
                .clicked()
            {
                out.push(Command::Delete);
            }
        });
    });
    ui.add_space(4.0);
    ui.separator();
}

/// One tool on the bar: its icon, and its word and key where there is room.
fn tool(icon: &'static str, label: &str, hint: &str, labels: bool) -> Button<'static> {
    match labels {
        true => Button::new(label).icon(icons::glyph(icon)).hint(hint).quiet(),
        false => Button::icon_only(icons::glyph(icon)).quiet(),
    }
}

/// A tool's tooltip: what it does, and the key it is also on.
pub fn tip(what: &str, key: &str) -> String {
    match key {
        "" => what.to_string(),
        key => format!("{what}  ({key})"),
    }
}

/// The open message's header: the sender's initial, name and address, the
/// account it came to in that account's colour, and when. Clicking it closes
/// the message, as clicking its line would.
fn sender_block(ui: &mut Ui, message: &Message, account: &str, colour: Color32, follow: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 60.0), Sense::click());
    if follow {
        ui.scroll_to_rect(rect, None);
    }
    let visuals = ui.visuals();
    let (strong, text, weak) = (
        visuals.strong_text_color(),
        visuals.text_color(),
        visuals.weak_text_color(),
    );
    let (name, address) = split_sender(&message.from);

    let centre = Pos2::new(rect.min.x + 22.0, rect.center().y);
    ui.painter().circle_filled(centre, 21.0, visuals.faint_bg_color);
    ui.painter().text(
        centre,
        Align2::CENTER_CENTER,
        sidebar::initial(name),
        FontId::proportional(15.0),
        text,
    );

    let date_left = ui
        .painter()
        .text(
            Pos2::new(rect.max.x - 4.0, rect.min.y + 18.0),
            Align2::RIGHT_CENTER,
            &message.date,
            FontId::monospace(12.0),
            weak,
        )
        .min
        .x;
    let x = rect.min.x + 56.0;
    let painter = ui.painter().with_clip_rect(Rect::from_min_max(
        Pos2::new(x, rect.min.y),
        Pos2::new(date_left - 12.0, rect.max.y),
    ));
    let first = rect.min.y + 18.0;
    let font = FontId::proportional(14.5);
    let at = Pos2::new(x, first);
    let end = painter.text(at, Align2::LEFT_CENTER, name, font.clone(), strong).max.x;
    painter.text(at + Vec2::new(0.35, 0.0), Align2::LEFT_CENTER, name, font, strong);
    if address != name {
        painter.text(
            Pos2::new(end + 6.0, first),
            Align2::LEFT_CENTER,
            format!("<{address}>"),
            FontId::proportional(13.5),
            weak,
        );
    }
    let second = rect.min.y + 41.0;
    let to = painter
        .text(Pos2::new(x, second), Align2::LEFT_CENTER, "to", FontId::proportional(13.0), weak)
        .max
        .x;
    painter.rect_filled(
        Rect::from_center_size(Pos2::new(to + 11.0, second), Vec2::splat(9.0)),
        2.0,
        colour,
    );
    painter.text(
        Pos2::new(to + 21.0, second),
        Align2::LEFT_CENTER,
        account,
        FontId::proportional(13.0),
        weak,
    );
    resp.on_hover_text("Close this message").clicked()
}

/// "Name <address>" as its two halves. A bare address is its own name.
fn split_sender(from: &str) -> (&str, &str) {
    match from.split_once('<') {
        Some((name, rest)) => {
            let address = rest.trim_end_matches('>').trim();
            let name = name.trim().trim_matches('"').trim();
            (if name.is_empty() { address } else { name }, address)
        }
        None => (from.trim(), from.trim()),
    }
}

/// One message's line: who wrote it and when, with the open one carried by the
/// selection colour and unread by weight. Returns true when it was clicked.
fn attribution(
    ui: &mut eframe::egui::Ui,
    message: &Message,
    open: bool,
    focused: bool,
    follow: bool,
) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), LINE_H), Sense::click());
    if follow {
        ui.scroll_to_rect(rect, None);
    }
    Item {
        open: false,
        keyed: focused,
        hovered: resp.hovered(),
        size: Size::Regular,
    }
    .paint(ui, rect);
    let visuals = ui.visuals();
    let color = if open || message.unread {
        visuals.strong_text_color()
    } else {
        visuals.text_color()
    };
    let weak = visuals.weak_text_color();

    let who = Rect::from_min_max(
        Pos2::new(rect.min.x + LINE_X, rect.min.y),
        Pos2::new((rect.max.x - LINE_DATE_W).max(rect.min.x + LINE_X + 1.0), rect.max.y),
    );
    let font = FontId::proportional(13.0);
    let painter = ui.painter().with_clip_rect(who);
    let at = Pos2::new(who.min.x, rect.center().y);
    painter.text(at, Align2::LEFT_CENTER, &message.from, font.clone(), color);
    if message.unread {
        painter.text(
            at + Vec2::new(0.35, 0.0),
            Align2::LEFT_CENTER,
            &message.from,
            font,
            color,
        );
    }
    // Where the body of an unopened message would be: its first line, quiet,
    // so the conversation reads as a conversation rather than a column of
    // names.
    if !open {
        let said: String = message
            .text
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .chars()
            .take(140)
            .collect();
        let wrote = painter
            .layout_no_wrap(message.from.clone(), FontId::proportional(13.0), color)
            .size()
            .x;
        painter.text(
            Pos2::new(who.min.x + wrote + 10.0, rect.center().y),
            Align2::LEFT_CENTER,
            said,
            FontId::proportional(12.0),
            weak,
        );
    }
    ui.painter().text(
        Pos2::new(rect.max.x - 8.0, rect.center().y),
        Align2::RIGHT_CENTER,
        &message.date,
        FontId::proportional(12.0),
        weak,
    );
    resp.on_hover_text(if open {
        "Close this message"
    } else {
        "Open this message"
    })
    .clicked()
}

/// The body as the mail library extracted it — what a plain-text message says,
/// and what an HTML one says once it is stripped. It is egui text, so it lays
/// out where it falls and the conversation scrolls over it.
fn text_body(ui: &mut eframe::egui::Ui, message: &Message) {
    ui.add_space(6.0);
    if message.text.trim().is_empty() {
        ui.label(RichText::new("This message has no body.").weak());
    } else {
        ui.label(RichText::new(&message.text).size(13.5));
    }
    ui.add_space(6.0);
}

/// What a click on an attachment card asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Open,
    Save,
    SaveAs,
}

const CARD_W: f32 = 270.0;
const CARD_H: f32 = 44.0;
const CARD_PAD: f32 = 10.0;
const CARD_ICON: f32 = 26.0;

/// One attachment as a card: a type badge, the name over its size, and Save
/// and Save as at the right. The card itself opens the file.
fn attachment_card(ui: &mut eframe::egui::Ui, file: &crate::client::Attachment) -> Option<Pick> {
    use eframe::egui::{text::LayoutJob, text::TextWrapping, StrokeKind};

    let (rect, card) = ui.allocate_exact_size(Vec2::new(CARD_W, CARD_H), Sense::click());
    // The icons are registered after the card, so they sit on top of it and
    // take their own clicks.
    let save_rect = Rect::from_center_size(
        Pos2::new(rect.max.x - CARD_PAD - CARD_ICON * 1.5 - 4.0, rect.center().y),
        Vec2::splat(CARD_ICON),
    );
    let save_as_rect = Rect::from_center_size(
        Pos2::new(rect.max.x - CARD_PAD - CARD_ICON * 0.5, rect.center().y),
        Vec2::splat(CARD_ICON),
    );
    let save = ui.interact(save_rect, card.id.with("save"), Sense::click());
    let save_as = ui.interact(save_as_rect, card.id.with("save_as"), Sense::click());

    let visuals = ui.visuals().clone();
    let painter = ui.painter();
    let fill = if card.hovered() && !save.hovered() && !save_as.hovered() {
        visuals.widgets.hovered.weak_bg_fill
    } else {
        visuals.faint_bg_color
    };
    // A card is an item: the regular corner. What sits on it — the badge, the
    // marks under Save and Save as — the small one.
    painter.rect_filled(rect, Size::Regular.radius(), fill);
    painter.rect_stroke(
        rect,
        Size::Regular.radius(),
        visuals.widgets.noninteractive.bg_stroke,
        StrokeKind::Inside,
    );

    // The badge: the file's type, tinted by kind.
    let (label, hue) = kind(file);
    let badge_font = FontId::proportional(10.5);
    let badge_w = painter
        .layout_no_wrap(label.clone(), badge_font.clone(), hue)
        .size()
        .x
        + 10.0;
    let badge = Rect::from_min_size(
        Pos2::new(rect.min.x + CARD_PAD, rect.center().y - 9.0),
        Vec2::new(badge_w, 18.0),
    );
    painter.rect_filled(badge, Size::Small.radius(), hue.gamma_multiply(0.22));
    painter.text(badge.center(), Align2::CENTER_CENTER, label, badge_font, tint(hue, visuals.dark_mode));

    // The name, cut to the room it has, over its size.
    let text_x = badge.max.x + 10.0;
    let text_w = (save_rect.min.x - 8.0 - text_x).max(10.0);
    let mut job = LayoutJob::simple_singleline(
        file.name.clone(),
        FontId::proportional(13.0),
        visuals.text_color(),
    );
    job.wrap = TextWrapping::truncate_at_width(text_w);
    let galley = painter.layout_job(job);
    painter.galley(Pos2::new(text_x, rect.min.y + 6.0), galley, visuals.text_color());
    painter.text(
        Pos2::new(text_x, rect.max.y - 6.0),
        Align2::LEFT_BOTTOM,
        size(file.bytes),
        FontId::proportional(11.0),
        visuals.weak_text_color(),
    );

    // Save and Save as: quiet until the pointer is on them.
    for (r, response, icon) in [(save_rect, &save, "download"), (save_as_rect, &save_as, "save_as")] {
        let color = if response.hovered() {
            painter.rect_filled(r, Size::Small.radius(), visuals.widgets.hovered.bg_fill);
            visuals.strong_text_color()
        } else {
            visuals.weak_text_color()
        };
        crate::ui::icons::draw(painter, r.shrink(5.0), icon, color);
    }
    if save.hovered() {
        tip_above(ui, card.id.with("save_tip"), save_rect, "Save");
    }
    if save_as.hovered() {
        tip_above(ui, card.id.with("save_as_tip"), save_as_rect, "Save as…");
    }

    if save.clicked() {
        Some(Pick::Save)
    } else if save_as.clicked() {
        Some(Pick::SaveAs)
    } else if card.clicked() {
        Some(Pick::Open)
    } else {
        None
    }
}

/// A tooltip opened above `anchor`, never below it: under the attachment row
/// is the body pane, a native view that would cover it.
fn tip_above(ui: &eframe::egui::Ui, id: eframe::egui::Id, anchor: Rect, text: &str) {
    use eframe::egui::{Area, Frame, Order};
    Area::new(id)
        .order(Order::Tooltip)
        .pivot(Align2::CENTER_BOTTOM)
        .fixed_pos(Pos2::new(anchor.center().x, anchor.min.y - 4.0))
        .interactable(false)
        .show(ui.ctx(), |ui| {
            Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(text);
            });
        });
}

/// The badge text and hue for a file: its extension, coloured by family.
fn kind(file: &crate::client::Attachment) -> (String, eframe::egui::Color32) {
    use eframe::egui::Color32;
    let ext = std::path::Path::new(&file.name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let label = if ext.is_empty() || ext.len() > 4 {
        "FILE".to_string()
    } else {
        ext.to_ascii_uppercase()
    };
    let hue = match ext.as_str() {
        "pdf" => Color32::from_rgb(0xE2, 0x4B, 0x4A),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "svg" => Color32::from_rgb(0x1D, 0x9E, 0x75),
        "doc" | "docx" | "odt" | "rtf" | "txt" | "md" => Color32::from_rgb(0x37, 0x8A, 0xDD),
        "xls" | "xlsx" | "ods" | "csv" => Color32::from_rgb(0x63, 0x99, 0x22),
        "ppt" | "pptx" | "odp" | "key" => Color32::from_rgb(0xD8, 0x5A, 0x30),
        _ if file.mime.starts_with("image/") => Color32::from_rgb(0x1D, 0x9E, 0x75),
        _ => Color32::from_rgb(0x88, 0x87, 0x80),
    };
    (label, hue)
}

/// The badge's text colour: the hue, lifted on a dark theme and deepened on
/// a light one, so it reads on its own tint.
fn tint(hue: eframe::egui::Color32, dark: bool) -> eframe::egui::Color32 {
    let toward = if dark { 255.0 } else { 0.0 };
    let mix = |c: u8| (c as f32 + (toward - c as f32) * 0.35) as u8;
    eframe::egui::Color32::from_rgb(mix(hue.r()), mix(hue.g()), mix(hue.b()))
}

/// A size the way people read it.
fn size(bytes: u64) -> String {
    match bytes {
        0 => String::new(),
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{} KB", (b + 512) / 1024),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}
