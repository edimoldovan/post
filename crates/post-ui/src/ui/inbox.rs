//! A list: one page of one account's mail as conversations, newest first.
//!
//! A row is one conversation, not one message: who it is from and when it
//! arrived on the first line, with a quiet count where there is more than one
//! message behind it; the subject they share on the second; the start of the
//! newest message on the third.
//!
//! A row is painted rather than assembled out of widgets, the way Grido paints
//! its grid: the whole row is one click target, and a row swiped left with
//! two fingers goes to the trash, as on a phone. Unread is carried by weight — the
//! app's font has one face, so the glyphs are struck twice a third of a pixel
//! apart, which is what a bold face does — and by a dot in the margin, never by
//! a badge.

use eframe::egui::text::{LayoutJob, TextFormat, TextWrapping};
use eframe::egui::{
    Align, Align2, Color32, Event, FontId, Id, Key, Layout, MouseWheelUnit, Painter, Pos2, Rect,
    RichText, ScrollArea, Sense, Stroke, Ui, Vec2,
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

/// The count chip after the sender, where a conversation has more than one
/// message on it. An outline and a number: quiet enough to read past, there
/// enough to say how deep the conversation is.
const CHIP_H: f32 = 15.0;
const CHIP_PAD: f32 = 5.0;
const CHIP_GAP: f32 = 6.0;

/// How far a row has to be swiped left, as a share of its width, for letting
/// go to put it in the trash. Short of it, the row slides back.
const SWIPE: f32 = 0.5;

/// A row let go short of the cut slides back into place; in seconds, quicker
/// than the eye takes it in as a wait.
const SLIDE_BACK: f32 = 0.1;

/// A row thrown away by a swipe slides the rest of the way out, then closes
/// up so the rows under it rise into its place; in seconds.
const SLIDE_OUT: f32 = 0.18;
const CLOSE_UP: f32 = 0.22;

/// How long a thrown-away row stays hidden while the trash is asked, in
/// seconds: a delete that fails shows the row again.
const GONE_FOR: f64 = 10.0;

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

/// A row being swiped left with two fingers: which one, how far, how wide it
/// is, and when the fingers let go of it short of the cut, while it slides
/// back.
#[derive(Clone, Default)]
struct Swipe {
    row: String,
    x: f32,
    width: f32,
    back: Option<f64>,
}

/// The touchpad's two-finger motion this frame, and whether the fingers left
/// it: a touchpad's scroll ends with a motion of nothing. A mouse wheel is no
/// swipe.
fn fingers(ui: &Ui) -> (Vec2, bool) {
    ui.input(|i| {
        let mut motion = Vec2::ZERO;
        let mut lifted = false;
        for event in &i.events {
            if let Event::MouseWheel { unit: MouseWheelUnit::Point, delta, .. } = event {
                motion += *delta;
                lifted |= *delta == Vec2::ZERO;
            }
        }
        (motion, lifted)
    })
}

/// Fast at first, easing to a stop: `p` from 0 to 1.
fn ease(p: f32) -> f32 {
    1.0 - (1.0 - p.clamp(0.0, 1.0)).powi(3)
}

impl Swipe {
    /// The fingers' motion over the row `row` this frame.
    fn track(&mut self, row: &str, dx: f32, dy: f32, width: f32, now: f64) {
        // A scroll that wobbles sideways is not a swipe.
        if dx == 0.0 || dx.abs() <= dy.abs() {
            return;
        }
        if self.row != row {
            *self = Swipe { row: row.to_string(), ..Swipe::default() };
        }
        // Caught again on its way back: from where it is drawn.
        self.x = self.shift(row, now);
        self.back = None;
        // Natural scrolling, as the touchpad is set: fingers moving left move
        // the content left.
        self.x = (self.x + dx).clamp(-width, 0.0);
        self.width = width;
    }

    /// The swipe once the fingers have left the touchpad, if it went past the
    /// cut and throws its row away. Short of it, the row slides back. Until
    /// they leave, nothing happens, however still they are.
    fn ended(&mut self, lifted: bool, now: f64) -> Option<Swipe> {
        if let Some(at) = self.back {
            if now - at >= f64::from(SLIDE_BACK) {
                *self = Swipe::default();
            }
            return None;
        }
        if !lifted || self.x == 0.0 {
            return None;
        }
        if !self.far() {
            self.back = Some(now);
            return None;
        }
        Some(std::mem::take(self))
    }

    /// Past the cut: letting go throws the row away.
    fn far(&self) -> bool {
        self.x < -self.width * SWIPE
    }

    /// How far left the row `row` is drawn at `now`.
    fn shift(&self, row: &str, now: f64) -> f32 {
        if self.row != row {
            return 0.0;
        }
        match self.back {
            Some(at) => self.x * (1.0 - ease((now - at) as f32 / SLIDE_BACK)),
            None => self.x,
        }
    }

    /// The row this swipe threw away, `t` seconds after: how far left it is
    /// and how tall. It slides out from where the fingers left it, then
    /// closes up.
    fn leaving(&self, t: f32) -> (f32, f32) {
        let x = self.x + (-self.width - self.x) * ease(t / SLIDE_OUT);
        (x, ROW_H * (1.0 - ease((t - SLIDE_OUT) / CLOSE_UP)))
    }
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
        // A row thrown away by a swipe stays hidden until the trash has it.
        let swipe_id = Id::new(("post-swipe", step));
        let gone_id = Id::new(("post-gone", step));
        let mut swipe: Swipe = ui.ctx().data_mut(|d| d.get_temp(swipe_id)).unwrap_or_default();
        let mut gone: Vec<(Swipe, f64)> = ui.ctx().data_mut(|d| d.get_temp(gone_id)).unwrap_or_default();
        let now = ui.input(|input| input.time);
        gone.retain(|(thrown, at)| now - at < GONE_FOR && rows.iter().any(|row| row.id == thrown.row));
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
                    let leaving = gone
                        .iter()
                        .find(|(thrown, _)| thrown.row == row.id)
                        .map(|(thrown, at)| thrown.leaving((now - at) as f32));
                    if let Some(what) = message_row(ui, row, shown, keyed, keyed && follow, &keys, &mut swipe, leaving) {
                        focused = Some(i);
                        hit = Some((what, row.clone()));
                    }
                }
                if paging {
                    loading_row(ui);
                }
            });
        // The fingers leaving the touchpad throw the swiped row away, or let
        // it slide back.
        if let Some(thrown) = swipe.ended(fingers(ui).1, now) {
            if let Some(row) = rows.iter().find(|row| row.id == thrown.row) {
                hit = Some((Hit::Delete, row.clone()));
            }
            gone.push((thrown, now));
        }
        // Drawn every frame while a row is on its way out or back.
        if swipe.back.is_some() || gone.iter().any(|(_, at)| now - at < f64::from(SLIDE_OUT + CLOSE_UP)) {
            ui.ctx().request_repaint();
        } else if !gone.is_empty() {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
        }
        ui.ctx().data_mut(|d| {
            d.insert_temp(swipe_id, swipe);
            d.insert_temp(gone_id, gone);
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
            // The line is the conversation, so throwing it away takes the
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
///
/// `swipe` follows two fingers moving left over the row: the row slides with
/// them over the trash.
/// `leaving` is a row already thrown away: how far out it is and how tall.
#[allow(clippy::too_many_arguments)]
fn message_row(
    ui: &mut eframe::egui::Ui,
    row: &Thread,
    open: bool,
    focused: bool,
    follow: bool,
    keys: &RowKeys,
    swipe: &mut Swipe,
    leaving: Option<(f32, f32)>,
) -> Option<Hit> {
    let height = leaving.map_or(ROW_H, |(_, h)| h);
    if height <= 0.0 {
        return None;
    }
    let sense = if leaving.is_some() { Sense::hover() } else { Sense::click() };
    let (full, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), sense);
    if follow && leaving.is_none() {
        ui.scroll_to_rect(full, None);
    }
    if leaving.is_none() && resp.contains_pointer() {
        let motion = fingers(ui).0;
        swipe.track(&row.id, motion.x, motion.y, full.width(), ui.input(|i| i.time));
    }
    let x = leaving.map_or_else(|| swipe.shift(&row.id, ui.input(|i| i.time)), |(x, _)| x);
    // A row closing up shows only what is left of it.
    let clip = ui.clip_rect();
    ui.set_clip_rect(full.intersect(clip));
    if x < 0.0 {
        let strip = Rect::from_min_max(Pos2::new(full.max.x + x, full.min.y), full.max)
            .shrink2(Vec2::new(0.0, 1.0));
        let fill = omarchy_kit::danger_fill(ui);
        let painter = ui.painter().with_clip_rect(strip);
        painter.rect_filled(strip, 0.0, fill);
        icons::draw(
            &painter,
            Rect::from_center_size(strip.center(), Vec2::splat(17.0)),
            "trash",
            omarchy_kit::on(fill),
        );
    }
    let rect = Rect::from_min_size(full.min + Vec2::new(x, 0.0), Vec2::new(full.width(), ROW_H));
    let hovered = resp.hovered() && x == 0.0;
    let (first, second, third) = (rect.min.y + 19.0, rect.min.y + 37.0, rect.min.y + 55.0);

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
        full.x_range(),
        full.max.y - 0.5,
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

    // The subject, and the start of the message under it.
    let right = rect.max.x - PAD_R;
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
    ui.set_clip_rect(clip);
    if leaving.is_some() {
        return None;
    }

    let resp = resp.on_hover_text("Open this conversation");

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
    Some(Hit::Open)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Two fingers left past half the row throw it away when they leave the
    /// touchpad, and not before, however still they are; short of it the row
    /// slides back, and a scroll with a sideways wobble moves nothing.
    #[test]
    fn a_row_swiped_past_half_goes_to_the_trash_when_let_go() {
        let mut swipe = Swipe::default();
        swipe.track("a", -200.0, 0.0, 300.0, 1.0);
        assert!(swipe.ended(false, 5.0).is_none(), "the fingers are still down");
        assert_eq!(swipe.shift("a", 5.0), -200.0);
        assert_eq!(swipe.ended(true, 5.0).map(|thrown| thrown.row).as_deref(), Some("a"));
        assert_eq!(swipe.shift("a", 5.0), 0.0, "the swipe is over");

        swipe.track("a", -140.0, 0.0, 300.0, 6.0);
        assert!(swipe.ended(true, 6.0).is_none(), "short of the cut");
        assert_eq!(swipe.shift("a", 6.0), -140.0, "it slides back from where it was let go");
        assert!(swipe.ended(false, 6.5).is_none());
        assert_eq!(swipe.shift("a", 6.5), 0.0, "and is back in place");

        swipe.track("a", -20.0, 40.0, 300.0, 7.0);
        assert_eq!(swipe.shift("a", 7.0), 0.0, "a scroll down is not a swipe");

        swipe.track("a", -200.0, 0.0, 300.0, 8.0);
        swipe.track("a", 100.0, 0.0, 300.0, 8.05);
        assert!(swipe.ended(true, 8.1).is_none(), "swiped back before letting go");
    }

    /// The touchpad's motion is what it scrolled; its scroll of nothing is
    /// the fingers leaving it. A mouse wheel is neither.
    #[test]
    fn a_touchpad_scroll_ends_with_a_motion_of_nothing() {
        let wheel = |unit, delta| Event::MouseWheel { unit, delta, modifiers: Default::default() };
        let ctx = eframe::egui::Context::default();
        let mut seen = Vec::new();
        let input = eframe::egui::RawInput {
            events: vec![
                wheel(MouseWheelUnit::Point, Vec2::new(-30.0, 2.0)),
                wheel(MouseWheelUnit::Line, Vec2::new(-1.0, 0.0)),
                wheel(MouseWheelUnit::Point, Vec2::ZERO),
            ],
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            eframe::egui::CentralPanel::default().show(ctx, |ui| seen.push(fingers(ui)));
        });
        assert_eq!(seen, vec![(Vec2::new(-30.0, 2.0), true)]);
    }

    /// A thrown-away row slides out from where the fingers left it, then
    /// closes up.
    #[test]
    fn a_thrown_row_slides_out_then_closes_up() {
        let thrown = Swipe { row: "a".into(), x: -150.0, width: 300.0, back: None };
        assert_eq!(thrown.leaving(0.0), (-150.0, ROW_H));
        assert_eq!(thrown.leaving(SLIDE_OUT), (-300.0, ROW_H));
        assert_eq!(thrown.leaving(SLIDE_OUT + CLOSE_UP).1, 0.0);
    }
}
