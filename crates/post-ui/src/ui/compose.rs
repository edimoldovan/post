//! Writing: a reply, a new email, or a Gmail draft opened again.
//!
//! Top-down, the way the reader works through it: the header — back, what
//! this is, Discard and Send — then who it is from, who it goes to, what it is
//! about, the files going with it, Attach, and the body. Under a reply's body
//! is the message it answers, quieter: read, never written. The same words go
//! out under the reply behind `> `, and the mail library is what puts them
//! there.
//!
//! The addresses are chips. What is typed becomes one on a space, a comma,
//! Enter or leaving the field; its ✕ takes it off, and Backspace in an empty
//! field takes the last one back. Cc and Bcc wait behind a toggle until they
//! are wanted. The body is plain text, with its links found as they are typed
//! and shown as links — nothing else is formatting.
//!
//! What is typed is saved into Gmail's Drafts once typing pauses and when the
//! screen is left, so it is in Gmail on every device; the header says so. Two
//! chords are taken before the editor sees them, because an editor with the
//! focus eats every key: Ctrl+Enter sends and Escape goes back.

use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{
    Align, CentralPanel, Color32, Context, FontSelection, Frame, Galley, Key, Layout, Modifiers,
    Popup, Response, RichText, ScrollArea, Stroke, TextBuffer, TextEdit, Ui, Vec2,
};
use omarchy_kit::{self as kit, Button, Size, TagField, TextField};

use crate::client::Job;
use crate::commands::Command;
use crate::route::Route;
use crate::state::{PostApp, Screen};
use crate::ui::icons;

/// The box the label in front of each line sits in, so From, To, Cc, Bcc and
/// Subject all start at the same place.
const LABEL: f32 = 64.0;

impl PostApp {
    pub fn compose_view(&mut self, ctx: &Context, account: &str) -> Vec<Command> {
        let mut out = Vec::new();
        let route = self.nav.top().clone();
        let reply = matches!(route, Route::Reply { .. });
        // A reply and a forward go from the account the message came to.
        let fixed = matches!(route, Route::Reply { .. } | Route::Forward { .. });
        // Before the editor is built, so a focused text field cannot swallow
        // the only two keys that mean something here — unless the palette is
        // open over it, and they are the palette's.
        if !self.command_palette.is_open() {
            ctx.input_mut(|input| {
                if input.consume_key(Modifiers::COMMAND, Key::Enter) {
                    out.push(Command::Send);
                }
                if input.consume_key(Modifiers::NONE, Key::Escape) {
                    out.push(Command::Back);
                }
            });
        }

        let accounts = self.accounts.clone().unwrap_or_default();
        let names: Vec<String> = accounts.iter().map(|a| self.name_of(a)).collect();
        let from = self.writing_account().unwrap_or_else(|| account.to_string());
        let from_name = self.name_of(&from);
        let mut picked = from.clone();
        let accent = self.palette.accent;
        let before = fingerprint(&self.screen);
        let (sending, saving) = (self.screen.sending, self.screen.saving);
        let saved = if saving {
            "Saving…"
        } else if self.screen.draft_id.is_some() && !self.screen.dirty {
            "Saved to Drafts"
        } else {
            ""
        };
        let title = match route {
            Route::Reply { .. } => "Reply",
            Route::Forward { .. } => "Forward",
            Route::Draft { .. } => "Draft",
            _ => "New message",
        };
        // The message being answered can be a mail's worth of text, so it
        // steps out of `self` for the frame rather than being copied into it.
        let draft = self.screen.draft.take();

        CentralPanel::default().show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .add(Button::icon_only(icons::glyph("back")).quiet().tip("Back (Esc) — what is typed stays in Drafts"))
                    .clicked()
                {
                    out.push(Command::Back);
                }
                ui.label(RichText::new(title).size(17.0).strong());
                if !saved.is_empty() {
                    ui.label(RichText::new(saved).size(12.0).weak());
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let send = Button::new(if sending { "Sending…" } else { "Send" })
                        .icon(icons::glyph("send"))
                        .primary();
                    if ui.add_enabled(!sending, send).on_hover_text("Send (Ctrl+Enter)").clicked() {
                        out.push(Command::Send);
                    }
                    if ui
                        .add_enabled(!sending, Button::new("Discard"))
                        .on_hover_text("Delete this draft for good")
                        .clicked()
                    {
                        out.push(Command::Discard);
                    }
                });
            });
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(2.0);

            // Who it is from: any connected account for a new email, and the
            // account it came to for a reply or a forward.
            ui.horizontal(|ui| {
                name_of(ui, "From");
                if fixed || accounts.len() < 2 {
                    value(ui, &from_name).on_hover_text(&from);
                } else {
                    // A button saying who it is from, and the accounts in a
                    // menu under it.
                    let shown = accounts
                        .iter()
                        .position(|a| *a == picked)
                        .map_or(picked.clone(), |i| names[i].clone());
                    let from = ui.add_enabled(!saving && !sending, Button::new(shown).hint("▾"));
                    Popup::menu(&from).show(|ui| {
                        for (account, name) in accounts.iter().zip(&names) {
                            let chosen = *account == picked;
                            if ui
                                .add(Button::new(name).quiet().selected(chosen))
                                .on_hover_text(account)
                                .clicked()
                            {
                                picked = account.clone();
                                ui.close();
                            }
                        }
                    });
                }
                if !self.screen.copies {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(Button::new("Cc / Bcc").quiet().small()).clicked() {
                            self.screen.copies = true;
                        }
                    });
                }
            });
            let s = &mut self.screen;
            let [to_typing, cc_typing, bcc_typing] = &mut s.typing;
            let to = address_line(ui, "To", &mut s.to, to_typing, "post-to");
            if s.copies {
                address_line(ui, "Cc", &mut s.cc, cc_typing, "post-cc");
                address_line(ui, "Bcc", &mut s.bcc, bcc_typing, "post-bcc");
            }
            match (&route, &draft) {
                (Route::Reply { .. }, Some(draft)) => field(ui, "Subject", &draft.subject),
                (Route::Reply { .. }, None) => field(ui, "Subject", "Working out who this answers…"),
                _ => text_line(ui, "Subject", &mut s.subject),
            }
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);

            // What goes with it, then Attach, above the body.
            if !s.attach.is_empty() {
                attachment_cards(ui, &mut s.attach);
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!sending, Button::new("Attach…").icon(icons::glyph("plus")))
                    .clicked()
                {
                    pick_attachments(self.picked_tx.clone(), ui.ctx().clone());
                }
            });
            ui.add_space(6.0);

            // The message being answered takes the lower part of the panel, so
            // there is always something on screen saying what this answers.
            let quoting = draft.as_ref().is_some_and(|d| !d.quote.trim().is_empty());
            let left = ui.available_height().max(60.0);
            let room = Vec2::new(
                ui.available_width(),
                if quoting { (left * 0.55).max(60.0) } else { left },
            );
            let mut layouter = |ui: &Ui, text: &dyn TextBuffer, wrap: f32| {
                highlighted(ui, text.as_str(), wrap, accent)
            };
            let body = kit::field(ui, Size::Regular, |ui| {
                let body = ui.add_sized(
                    room - Vec2::splat(2.0 * Size::Regular.pad()),
                    TextEdit::multiline(&mut self.screen.compose)
                        .id_salt("post-body")
                        .frame(false)
                        .hint_text(match route {
                            Route::Reply { .. } => "Write your reply",
                            Route::Forward { .. } => "Add a note, or send it as it is",
                            _ => "Write your message",
                        })
                        .desired_width(f32::INFINITY)
                        .layouter(&mut layouter),
                );
                (body.has_focus(), body)
            });
            // The keyboard starts where the writing does: in To for a new
            // email with nobody on it yet, in the body otherwise.
            if ui.memory(|m| m.focused().is_none()) {
                if !reply && self.screen.to.is_empty() && self.screen.typing[0].is_empty() {
                    to.request_focus();
                } else {
                    body.request_focus();
                }
            }

            // What this answers, under the body: labels, so there is nothing
            // here to type into and nothing to take the keyboard.
            if let Some(draft) = draft.as_ref().filter(|_| quoting) {
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);
                ScrollArea::vertical()
                    .id_salt("quoted")
                    .auto_shrink([false, false])
                    .max_height(ui.available_height().max(40.0))
                    .show(ui, |ui| {
                        ui.indent("quoted", |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(&draft.attribution).weak().size(12.5));
                            ui.add_space(2.0);
                            ui.label(RichText::new(&draft.quote).weak().size(12.5));
                        });
                    });
            }
        });
        self.screen.draft = draft;
        if picked != from {
            self.change_from(picked);
        }
        // Anything changed since the frame began is the reader's typing: it is
        // saved once they pause.
        if fingerprint(&self.screen) != before {
            self.screen.dirty = true;
            self.screen.edited = ctx.input(|input| input.time);
        }
        out
    }

    /// Another account to send from. A draft lives in the account it was
    /// saved in, so it moves with the sender: the old one goes, and the next
    /// save writes it into the new account's Drafts.
    pub fn change_from(&mut self, account: String) {
        if let (Some(before), Some(draft_id)) = (self.writing_account(), self.screen.draft_id.take()) {
            self.start(Job::DeleteDraft {
                account: before,
                draft_id,
            });
        }
        self.screen.from = Some(account);
    }
}

/// Files to go with what is being written, from the file picker — on its own
/// thread: see `PostApp::picked_tx`.
pub fn pick_attachments(picked: std::sync::mpsc::Sender<crate::state::Picked>, ctx: Context) {
    std::thread::spawn(move || {
        if let Some(files) = rfd::FileDialog::new().pick_files() {
            let files = files.iter().map(|f| f.to_string_lossy().into_owned()).collect();
            let _ = picked.send(crate::state::Picked::Attach(files));
            ctx.request_repaint();
        }
    });
}

/// Everything that is saved, folded into one number: a frame that changes it
/// is typing.
fn fingerprint(screen: &Screen) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (
        &screen.to,
        &screen.cc,
        &screen.bcc,
        &screen.subject,
        &screen.compose,
        &screen.attach,
        &screen.from,
    )
        .hash(&mut hasher);
    hasher.finish()
}

/// The body as the editor lays it out: plain text, with its links in the
/// accent and underlined.
fn highlighted(ui: &Ui, text: &str, wrap: f32, accent: Color32) -> Arc<Galley> {
    let font = FontSelection::default().resolve(ui.style());
    let plain = TextFormat::simple(font.clone(), ui.visuals().text_color());
    let link = TextFormat {
        underline: Stroke::new(1.0_f32, accent),
        ..TextFormat::simple(font, accent)
    };
    let mut job = LayoutJob::default();
    let mut at = 0;
    for range in links(text) {
        job.append(&text[at..range.start], 0.0, plain.clone());
        job.append(&text[range.clone()], 0.0, link.clone());
        at = range.end;
    }
    job.append(&text[at..], 0.0, plain);
    job.wrap.max_width = wrap;
    ui.fonts_mut(|fonts| fonts.layout_job(job))
}

/// Where the links are in a text: every run that starts `http://`, `https://`
/// or `www.`, to the next space — less the brackets around it and the
/// punctuation a sentence puts after it.
pub fn links(text: &str) -> Vec<Range<usize>> {
    const STARTS: [&str; 3] = ["https://", "http://", "www."];
    let mut found = Vec::new();
    let mut word: Option<usize> = None;
    for (i, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        if !c.is_whitespace() {
            word.get_or_insert(i);
            continue;
        }
        let Some(from) = word.take() else {
            continue;
        };
        let raw = &text[from..i];
        let trimmed = raw.trim_start_matches(['(', '<', '"', '\'', '[']);
        let lead = raw.len() - trimmed.len();
        let lower = trimmed.to_ascii_lowercase();
        let Some(start) = STARTS.iter().find(|start| lower.starts_with(*start)) else {
            continue;
        };
        let link = trimmed.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '>', '"', '\'', ']']);
        if link.len() > start.len() {
            found.push(from + lead..from + lead + link.len());
        }
    }
    found
}

/// The chips in an address field, in order.
pub fn chips(field: &str) -> Vec<&str> {
    field
        .split(',')
        .map(str::trim)
        .filter(|chip| !chip.is_empty())
        .collect()
}

/// True for what a field takes as an address: `ale@acme.com`, or
/// `Ale <ale@acme.com>` as a mailto link or a Gmail draft writes it.
pub fn is_address(text: &str) -> bool {
    let text = text.trim();
    let bare = match (text.rfind('<'), text.rfind('>')) {
        (Some(open), Some(close)) if open < close => &text[open + 1..close],
        _ => text,
    };
    let Some((local, domain)) = bare.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !bare.contains(char::is_whitespace)
        && domain.contains('.')
        && domain.split('.').all(|part| !part.is_empty())
        && !domain.contains('@')
}

/// What is being typed made a chip when it is an address — Enter, Tab,
/// leaving the field, Send. What is not one stays where it is, for the reader
/// to put right.
pub fn commit(field: &mut String, typing: &mut String) {
    let mut tags: Vec<String> = chips(field).into_iter().map(str::to_owned).collect();
    if TagField::commit(&mut tags, typing, is_address) {
        *field = tags.join(", ");
    }
}

/// One address line: its addresses as tags, then what is being typed, all
/// in one field that starts where the lines above and below do. A space, a
/// comma or a semicolon finishes an address. Returns the typing field, for
/// the keyboard to start in.
fn address_line(ui: &mut Ui, name: &str, field: &mut String, typing: &mut String, salt: &str) -> Response {
    ui.horizontal_top(|ui| {
        name_of(ui, name);
        let before: Vec<String> = chips(field).into_iter().map(str::to_owned).collect();
        let mut tags = before.clone();
        let line = ui.add(
            TagField::new(&mut tags, typing)
                .accept(is_address)
                .separators(" ,;")
                .hint("name@example.com")
                .id_salt(salt),
        );
        if tags != before {
            *field = tags.join(", ");
        }
        line
    })
    .inner
}

/// The files going with it, as cards: the name opens the file, its size is
/// beside it, and ✕ takes it off again.
fn attachment_cards(ui: &mut Ui, attach: &mut Vec<String>) {
    let mut dropped = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
        for (i, path) in attach.iter().enumerate() {
            let name = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            let size = std::fs::metadata(path).map(|m| size_of(m.len())).unwrap_or_default();
            Frame::group(ui.style()).corner_radius(Size::Regular.radius()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.link(name).on_hover_text(path).clicked() {
                        crate::state::open_file(path);
                    }
                    ui.label(RichText::new(size).weak().size(12.0));
                    if ui
                        .add(Button::icon_only(icons::glyph("close")).quiet().small().tip("Take it off"))
                        .clicked()
                    {
                        dropped = Some(i);
                    }
                });
            });
        }
    });
    if let Some(i) = dropped {
        attach.remove(i);
    }
}

/// A file's size, the way a card says it.
fn size_of(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{} KB", b.div_ceil(1024)),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

/// One line above the editor that is shown, not typed into.
fn field(ui: &mut Ui, name: &str, text: &str) {
    ui.horizontal(|ui| {
        name_of(ui, name);
        value(ui, text);
    });
}

/// A value that is only read, its text where a field's text starts.
fn value(ui: &mut Ui, text: &str) -> Response {
    ui.add_space(Size::Regular.pad() + 1.0);
    ui.label(RichText::new(text).size(13.5))
}

/// A plain line above the editor that is typed into: a new message's subject.
fn text_line(ui: &mut Ui, name: &str, value: &mut String) {
    ui.horizontal(|ui| {
        name_of(ui, name);
        ui.add(TextField::new(value).hint("What it is about"));
    });
}

/// The label in front of a line, in a box as wide as the widest and as tall
/// as a field: every field starts in the same place, its first line level
/// with the label.
fn name_of(ui: &mut Ui, name: &str) {
    let label = Vec2::new(LABEL, Size::Regular.height());
    ui.allocate_ui_with_layout(label, Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_size(label);
        ui.label(RichText::new(name).weak().size(13.0));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a field takes as an address, and what it does not.
    #[test]
    fn an_address_is_what_mail_can_be_sent_to() {
        for address in ["ale@acme.com", "Ale <ale@acme.com>", " cm@ey.co.uk "] {
            assert!(is_address(address), "{address}");
        }
        for not in ["ale", "ale@", "@acme.com", "ale@acme", "ale@acme.", "a le@acme.com", ""] {
            assert!(!is_address(not), "{not}");
        }
    }

    /// Send commits what is typed when it is an address — once — and leaves
    /// what is not one typed, for the reader to fix.
    #[test]
    fn what_is_typed_is_committed_when_it_is_an_address() {
        let (mut field, mut typing) = ("ale@acme.com".to_string(), "cm@ey.com".to_string());
        commit(&mut field, &mut typing);
        assert_eq!(chips(&field), vec!["ale@acme.com", "cm@ey.com"]);
        assert_eq!(typing, "");

        typing = "ALE@acme.com".to_string();
        commit(&mut field, &mut typing);
        assert_eq!(chips(&field).len(), 2, "the same address twice is one chip");

        typing = "fredrik".to_string();
        commit(&mut field, &mut typing);
        assert_eq!(typing, "fredrik", "not an address: it stays");
        assert_eq!(chips(&field).len(), 2);
    }

    /// The links in a body, as typed: the bracket and the full stop around
    /// one are not part of it.
    #[test]
    fn links_are_found_as_they_are_typed() {
        let text = "See https://status.hetzner.com. Or (www.acme.com/q?x=1) — not www.";
        let found: Vec<&str> = links(text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(found, vec!["https://status.hetzner.com", "www.acme.com/q?x=1"]);
        assert!(links("nothing here").is_empty());
        assert_eq!(links("http://ä.se").len(), 1, "a link can be more than ASCII");
    }

    /// A card says a file's size the way people do.
    #[test]
    fn a_size_reads_like_a_size() {
        assert_eq!(size_of(500), "500 B");
        assert_eq!(size_of(84 * 1024), "84 KB");
        assert_eq!(size_of(3 * 1024 * 1024 + 512 * 1024), "3.5 MB");
    }
}
