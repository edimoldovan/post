//! Letting Claude use Post: the question when Post first starts, and its
//! switch in Settings.

use eframe::egui::{Context, Frame, Id, Modal, RichText, Stroke, Ui};
use omarchy_kit::{Button, Size, Toggle};

use crate::state::PostApp;

/// What saying yes means, in the question and under the switch.
const WHAT: &str = "Claude can read, search and write your mail with Post's tools. \
                    Sending mail, deleting a draft and removing an account always ask \
                    you first.";

/// Room between the question's edge and what is in it.
const MARGIN: f32 = 16.0;

impl PostApp {
    /// The question, until it is answered: Post asks it when it first starts.
    pub fn claude_question(&mut self, ctx: &Context) {
        if self.claude.is_some() {
            return;
        }
        let mut answer = None;
        let width = (ctx.content_rect().width() - 2.0 * (16.0 + MARGIN)).min(420.0);
        Modal::new(Id::new("post-claude-question"))
            .frame(card(ctx))
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.label(RichText::new("Let Claude use Post?").size(17.0).strong());
                ui.add_space(8.0);
                ui.label(WHAT);
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Post adds itself to Claude Desktop and Claude Code. You can \
                         change this in Settings.",
                    )
                    .weak(),
                );
                ui.add_space(MARGIN);
                ui.horizontal(|ui| {
                    if ui.add(Button::new("Allow").primary()).clicked() {
                        answer = Some(true);
                    }
                    if ui.add(Button::new("Don't allow")).clicked() {
                        answer = Some(false);
                    }
                });
            });
        if let Some(allow) = answer {
            self.set_claude(allow);
        }
    }

    /// The switch in Settings, and what it means under it.
    pub fn claude_setting(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Claude").size(14.0));
            let mut on = usize::from(self.claude == Some(true));
            if ui.add(Toggle::new(&mut on, &["Off", "On"])).changed() {
                self.set_claude(on == 1);
            }
        });
        ui.add_space(4.0);
        ui.label(RichText::new(WHAT).weak().size(12.5));
    }

    /// Keeps the choice, and makes Claude's configs match it.
    fn set_claude(&mut self, allow: bool) {
        match post::claude::choose(allow) {
            Ok(()) => self.claude = Some(allow),
            Err(e) => self.set_status(format!("Claude: {e}")),
        }
    }
}

/// The question's card: the window's fill, a hairline, and a corner that
/// keeps the buttons inside it concentric.
fn card(ctx: &Context) -> Frame {
    let style = ctx.style();
    let v = &style.visuals;
    Frame::popup(&style)
        .fill(v.panel_fill)
        .stroke(Stroke::new(1.0_f32, omarchy_kit::theme::mix(v.panel_fill, v.text_color(), 0.14)))
        .corner_radius(Size::Regular.radius() + MARGIN)
        .inner_margin(MARGIN)
}
