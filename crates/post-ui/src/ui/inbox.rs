//! A list: one page of one account's mail as conversations, newest first.
//!
//! A row is one conversation, not one message: who it is from and when it
//! arrived on the first line, with a quiet count where there is more than one
//! message behind it; the subject they share on the second; the start of the
//! newest message on the third.
//!
//! A row is painted rather than assembled out of widgets, the way Grido paints
//! its grid: the whole row is one click target, with the trash a zone at its
//! right-hand end while the pointer is on it. Unread is carried by weight — the
//! app's font has one face, so the glyphs are struck twice a third of a pixel
//! apart, which is what a bold face does — and by a dot in the margin, never by
//! a badge.

use eframe::egui::text::{LayoutJob, TextFormat, TextWrapping};
use eframe::egui::{
    Align, Align2, Color32, FontId, Key, Layout, Painter, Pos2, Rect, RichText, ScrollArea, Sense,
    Stroke, Ui, Vec2,
};

use omarchy_kit::{Item, Size, TextField, Toggle, context_menu};

use crate::client::{Job, Thread};
use crate::commands::Command;
use crate::route::{INBOX, Route, query_of, searching};
use crate::state::{Pane, PostApp};
use crate::ui::icons;

const ROW_H: f32 = 72.0;
const DOT_X: f32 = 16.0;
const TEXT_X: f32 = 30.0;
const PAD_R: f32 = 14.0;
const TRASH_W: f32 = 30.0;

/// The count chip after the sender, where a conversation has more than one
/// message on it. An outline and a number: quiet enough to read past, there
/// enough to say how deep the conversation is.
const CHIP_H: f32 = 15.0;
const CHIP_PAD: f32 = 5.0;
const CHIP_GAP: f32 = 6.0;

/// How close to the end of the list the wheel has to get before the page after
/// it is asked for. About two rows: near enough to be arriving, far enough that
/// it is already there when the reader gets to the bottom.
const NEAR_END: f32 = ROW_H * 2.0;

/// What a click on a row, or a line of its menu, asked for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Open,
    Delete,
    Reply,
    ReplyAll,
    Forward,
    /// Read, or unread: the whole conversation.
    Mark(bool),
}

/// The keys the row menu's lines are also on, as the keymap has them.
struct RowKeys {
    open: String,
    reply: String,
    reply_all: String,
    forward: String,
    delete: String,
}

impl PostApp {
    /// The list of the step `step` — the screen on top, or the folder behind
    /// the message beside it — drawn into `ui`.
    pub fn list(&mut self, ui: &mut Ui, account: &str, step: u64) {
        let on_top = step == self.nav.key();
        // The keyboard's row is outlined only in the list the keyboard is in.
        let keyed_list = self.pane() == Pane::List && self.list_step() == Some(step);
        let name = self.name_of(account);
        let key = |cmd| {
            self.keymap
                .shortcut_for(cmd)
                .map(|shortcut| ui.ctx().format_shortcut(&shortcut))
                .unwrap_or_default()
        };
        let keys = RowKeys {
            open: key(Command::Open),
            reply: key(Command::Reply),
            reply_all: key(Command::ReplyAll),
            forward: key(Command::Forward),
            delete: key(Command::Delete),
        };
        let Some((route, screen)) = self.step_of(Some(step)) else {
            return;
        };
        let folder = route.folder().unwrap_or(INBOX).to_string();
        // The box says what a search searched for, until the reader types.
        let mut search = screen
            .search
            .get_or_insert_with(|| query_of(&folder).unwrap_or_default().to_string())
            .clone();
        let unread = screen.unread;
        let rows = screen
            .mailbox
            .as_ref()
            .map(|mailbox| mailbox.threads.clone())
            .unwrap_or_default();
        let focus = screen.focus;
        let loading = screen.mailbox.is_none();
        let paging = screen.paging;
        // Only the keyboard's list follows the keyboard.
        let follow = keyed_list && std::mem::take(&mut self.follow_focus);
        // The conversation open beside this list, if one is: its row is the
        // one filled. A list with nothing open beside it fills no row.
        let open = (!on_top)
            .then(|| self.nav.top().message().map(str::to_owned))
            .flatten();
        let focus_search = std::mem::take(&mut self.focus_search);
        // How many conversations in this folder are unread; a search has no
        // count of its own.
        let new = match query_of(&folder) {
            Some(_) => 0,
            None => self
                .folders
                .get(account)
                .and_then(|labels| labels.iter().find(|label| label.id == folder))
                .map_or(0, |label| label.unread),
        };
        let mut hit: Option<(Hit, Thread)> = None;
        let mut focused: Option<usize> = None;
        let mut searched: Option<String> = None;
        let mut filtered: Option<bool> = None;

        // The header: Gmail search in this account, All / Unread, the count.
        ui.add_space(10.0);
        let edit = ui.add(
            TextField::new(&mut search)
                .icon(icons::glyph("find"))
                .hint(format!("Search {name}"))
                .key("/"),
        );
        if focus_search {
            edit.request_focus();
        }
        if edit.lost_focus()
            && ui.input(|input| input.key_pressed(Key::Enter))
            && !search.trim().is_empty()
        {
            searched = Some(search.trim().to_string());
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let mut chosen = usize::from(unread);
            if ui.add(Toggle::new(&mut chosen, &["All", "Unread"])).changed() {
                filtered = Some(chosen == 1);
            }
            if new > 0 {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(format!("{new} new")).monospace().weak());
                });
            }
        });
        ui.add_space(4.0);
        ui.separator();
        if let Some((_, screen)) = self.step_of(Some(step)) {
            screen.search = Some(search);
        }
        if let Some(query) = searched {
            self.goto(Route::Folder {
                account: account.to_string(),
                folder: searching(&query),
            });
            return;
        }
        if let Some(unread) = filtered {
            self.filter(step, account, &folder, unread);
            return;
        }

        if loading {
            self.empty(ui, "Fetching this mailbox…");
            return;
        }
        if rows.is_empty() {
            self.empty(
                ui,
                if unread {
                    "Nothing unread here."
                } else {
                    "Nothing in this mailbox."
                },
            );
            return;
        }
        // Each step of the history scrolls on its own: Back finds the list
        // where it was left, and it is the same list beside a message.
        let out = ScrollArea::vertical()
            .id_salt(("post-list", step))
            .show(ui, |ui| {
                for (i, row) in rows.iter().enumerate() {
                    let shown = open
                        .as_deref()
                        .is_some_and(|id| row.id == id || row.ids.iter().any(|m| m == id));
                    let keyed = keyed_list && i == focus;
                    if let Some(what) = message_row(ui, row, shown, keyed, keyed && follow, &keys) {
                        focused = Some(i);
                        hit = Some((what, row.clone()));
                    }
                }
                if paging {
                    loading_row(ui);
                }
            });
        // The wheel arriving at the end of the list is the ask for the
        // page after it — and a list too short to fill the viewport asks
        // by itself, until the window is full or the mailbox is out.
        let seen = out.state.offset.y + out.inner_rect.height();
        let near_end = out.content_size.y <= out.inner_rect.height()
            || out.content_size.y - seen < NEAR_END;

        if near_end && !self.stalled {
            self.load_more_for(step);
        }
        if let Some(i) = focused
            && let Some((_, screen)) = self.step_of(Some(step))
        {
            screen.focus = i;
        }
        match hit {
            Some((Hit::Open, thread)) => {
                self.open_row(account, &thread);
                self.pane = Pane::Reader;
            }
            // The line is the conversation, so the trash on it takes the
            // conversation: every message behind that one line.
            Some((Hit::Delete, thread)) => {
                for id in &thread.ids {
                    self.delete(account, id);
                }
            }
            // A reply and a forward answer the newest message on it, as they
            // do from the reader.
            Some((what @ (Hit::Reply | Hit::ReplyAll), thread)) => {
                if let Some(id) = thread.ids.first() {
                    self.goto(Route::Reply {
                        account: account.to_string(),
                        folder: folder.clone(),
                        id: id.clone(),
                        all: what == Hit::ReplyAll,
                    });
                }
            }
            Some((Hit::Forward, thread)) => {
                if let Some(id) = thread.ids.first() {
                    self.goto(Route::Forward {
                        account: account.to_string(),
                        folder: folder.clone(),
                        id: id.clone(),
                    });
                }
            }
            Some((Hit::Mark(read), thread)) => {
                for id in &thread.ids {
                    self.start(Job::Mark {
                        account: account.to_string(),
                        id: id.clone(),
                        read,
                    });
                }
            }
            None => {}
        }
    }
}

impl PostApp {
    /// All or Unread, on the list of the step `step`: the folder is listed
    /// again, with only what is unread in it or with everything.
    pub fn filter(&mut self, step: u64, account: &str, folder: &str, unread: bool) {
        let Some((_, screen)) = self.step_of(Some(step)) else {
            return;
        };
        screen.unread = unread;
        screen.mailbox = None;
        screen.paging = false;
        screen.focus = 0;
        screen.asked = true;
        self.start_for(
            step,
            Job::List {
                account: account.to_string(),
                folder: folder.to_string(),
                unread,
                page: None,
            },
        );
    }
}

/// One conversation's row. Returns what was clicked, if anything.
///
/// `follow` is the keyboard having just landed here: the row asks the list to
/// scroll it into view, which does nothing when it is already there.
///
/// `open` is the conversation in the reader: its row is filled. `focused` is
/// the row the keyboard is on: an outline, so the arrows and Enter show where
/// they are without the row looking open.
fn message_row(
    ui: &mut eframe::egui::Ui,
    row: &Thread,
    open: bool,
    focused: bool,
    follow: bool,
    keys: &RowKeys,
) -> Option<Hit> {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
    if follow {
        ui.scroll_to_rect(rect, None);
    }
    let hovered = resp.hovered();
    let (first, second, third) = (rect.min.y + 19.0, rect.min.y + 37.0, rect.min.y + 55.0);
    let trash = Rect::from_center_size(
        Pos2::new(rect.max.x - PAD_R - TRASH_W / 2.0, (second + third) / 2.0),
        Vec2::splat(TRASH_W),
    );
    let on_trash = resp.hover_pos().is_some_and(|at| trash.contains(at));

    // Open, under the pointer, where the keyboard is: the kit's marks, clear
    // of the line between rows.
    Item {
        open,
        keyed: focused,
        hovered,
        size: Size::Regular,
    }
    .paint(ui, rect.shrink2(Vec2::new(0.0, 1.0)));
    let visuals = ui.visuals();
    ui.painter().hline(
        rect.x_range(),
        rect.max.y - 0.5,
        Stroke::new(1.0_f32, visuals.widgets.noninteractive.bg_stroke.color),
    );
    let fg = visuals.text_color();
    let strong = visuals.strong_text_color();
    let weak = visuals.weak_text_color();
    let accent = visuals.selection.stroke.color;
    let painter = ui.painter();

    if row.unread {
        painter.circle_filled(Pos2::new(rect.min.x + DOT_X, first), 4.0, accent);
    }

    // When, at the end of the first line; the sender has the rest of it.
    let date = painter.text(
        Pos2::new(rect.max.x - PAD_R, first),
        Align2::RIGHT_CENTER,
        &row.latest.date,
        FontId::monospace(11.5),
        weak,
    );
    let left = rect.min.x + TEXT_X;
    let wrote = elided(
        painter,
        Pos2::new(left, first),
        name_of(&row.who),
        14.0,
        if row.unread { strong } else { fg },
        row.unread,
        date.min.x - 10.0 - left,
    );
    // How many messages are behind this one line. One is no number at all: a
    // chip on every row is a column of ones.
    if row.count > 1 && wrote + CHIP_GAP + 24.0 < date.min.x {
        count_chip(painter, Pos2::new(wrote + CHIP_GAP, first), row.count, weak);
    }

    // The subject, and the start of the message under it. The trash is in
    // their way only while the pointer is on the row.
    let right = rect.max.x - PAD_R - if hovered { TRASH_W + 6.0 } else { 0.0 };
    elided(
        painter,
        Pos2::new(left, second),
        if row.subject.is_empty() {
            "(no subject)"
        } else {
            row.subject.as_str()
        },
        13.5,
        fg,
        row.unread,
        right - left,
    );
    elided(
        painter,
        Pos2::new(left, third),
        &row.latest.snippet,
        13.0,
        weak,
        false,
        right - left,
    );

    if hovered {
        icons::draw(
            painter,
            Rect::from_center_size(trash.center(), Vec2::splat(17.0)),
            "trash",
            if on_trash { accent } else { weak },
        );
    }

    let resp = resp.on_hover_text(if on_trash {
        "Move this conversation to the trash"
    } else {
        "Open this conversation"
    });

    // The right button: what every mail client offers on a line of its list.
    // A draft is only opened or thrown away.
    let mut chosen = None;
    let draft = !row.latest.draft_id.is_empty();
    context_menu(&resp, |m| {
        if m.item("Open").key(keys.open.clone()).clicked() {
            chosen = Some(Hit::Open);
        }
        if !draft {
            m.separator();
            if m.item("Reply").key(keys.reply.clone()).clicked() {
                chosen = Some(Hit::Reply);
            }
            if m.item("Reply all").key(keys.reply_all.clone()).clicked() {
                chosen = Some(Hit::ReplyAll);
            }
            if m.item("Forward").key(keys.forward.clone()).clicked() {
                chosen = Some(Hit::Forward);
            }
            m.separator();
            let (label, read) = if row.unread { ("Mark as read", true) } else { ("Mark as unread", false) };
            if m.item(label).clicked() {
                chosen = Some(Hit::Mark(read));
            }
        }
        m.separator();
        if m.item("Delete").key(keys.delete.clone()).clicked() {
            chosen = Some(Hit::Delete);
        }
    });
    if chosen.is_some() {
        return chosen;
    }
    if !resp.clicked() {
        return None;
    }
    Some(if on_trash { Hit::Delete } else { Hit::Open })
}

/// The line at the end of the list while the page after it is on the wire.
/// Quiet on purpose: it is a note that more is coming, not a row to click.
fn loading_row(ui: &mut eframe::egui::Ui) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::hover());
    ui.painter().text(
        Pos2::new(rect.min.x + TEXT_X, rect.center().y),
        Align2::LEFT_CENTER,
        "Loading…",
        FontId::proportional(12.5),
        ui.visuals().weak_text_color(),
    );
}

/// One line of text, cut with an ellipsis at `width`, optionally bold. The
/// bundled font has one face, so weight is a second strike a fraction of a
/// pixel across — which is what a bold face is.
///
/// Returns where the text ended, so whatever follows it on the line — the count
/// chip — knows where to start.
fn elided(
    painter: &Painter,
    at: Pos2,
    text: &str,
    size: f32,
    color: Color32,
    bold: bool,
    width: f32,
) -> f32 {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat::simple(FontId::proportional(size), color),
    );
    job.wrap = TextWrapping {
        max_width: width.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    let top_left = Pos2::new(at.x, at.y - galley.size().y / 2.0);
    let end = top_left.x + galley.size().x;
    if bold {
        painter.galley(top_left + Vec2::new(0.35, 0.0), galley.clone(), color);
    }
    painter.galley(top_left, galley, color);
    end
}

/// How many messages are on this conversation: a number in an outline, sitting
/// after the sender. No fill and no accent — it says "more than one", not
/// "look at me".
fn count_chip(painter: &Painter, at: Pos2, count: usize, color: Color32) {
    let text = count.to_string();
    let font = FontId::proportional(10.5);
    let width = painter
        .layout_no_wrap(text.clone(), font.clone(), color)
        .size()
        .x;
    let chip = Rect::from_min_size(
        Pos2::new(at.x, at.y - CHIP_H / 2.0),
        Vec2::new(width + CHIP_PAD * 2.0, CHIP_H),
    );
    painter.rect_stroke(
        chip,
        Size::Small.radius(),
        Stroke::new(1.0_f32, color),
        eframe::egui::StrokeKind::Inside,
    );
    painter.text(chip.center(), Align2::CENTER_CENTER, text, font, color);
}

/// The sender as a person: the display name when the address carries one,
/// the address alone otherwise. Nobody needs the email next to the name.
fn name_of(who: &str) -> &str {
    let name = who.split('<').next().unwrap_or(who).trim();
    let name = name.trim_matches('"').trim();
    // The self-thread line reads "To <address>" — keep it whole.
    if name == "To" {
        return who;
    }
    if name.is_empty() {
        who.trim_start_matches('<').trim_end_matches('>').trim()
    } else {
        name
    }
}
