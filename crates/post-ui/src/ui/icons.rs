//! Post's own icons: the sidebar's, the reader's bar's, the rows'.
//!
//! The shared set (`omarchy_kit::icons`) carries what every app needs —
//! the trash, find, save, the arrows. What is left here is mail vocabulary: an
//! envelope, a person, a plus, a back arrow, a reload. They are drawn with the
//! same [`Pen`], so they sit at the same weight and size as the shared ones.
//!
//! `draw` returns false for names neither set knows, letting the caller fall
//! back to rendering the name as text.

use eframe::egui::{Color32, Painter, Pos2, Rect, Shape};

use omarchy_kit::icons::{center_optically, Pen};

/// Draws `name` centered in `rect`. Returns false if there is no such icon.
/// Post's own glyphs win; anything else comes from the shared set.
pub fn draw(painter: &Painter, rect: Rect, name: &str, color: Color32) -> bool {
    let Some(mut shapes) =
        build(rect, name, color).or_else(|| omarchy_kit::icons::build(rect, name, color))
    else {
        return false;
    };
    center_optically(&mut shapes, rect);
    painter.add(Shape::Vec(shapes));
    true
}

/// One of Post's icons as the kit's widgets take one: drawn in the square and
/// the colour they give it.
pub fn glyph(name: &'static str) -> impl Fn(&Painter, Rect, Color32) {
    move |painter, rect, color| {
        draw(painter, rect, name, color);
    }
}

/// Builds a Post icon's shapes, or None for a name the mail set doesn't carry.
fn build(rect: Rect, name: &str, color: Color32) -> Option<Vec<Shape>> {
    let pen = Pen::new(rect, color);
    {
        let thin = pen.thin();
        let thick = pen.thick();
        let p = |x: f32, y: f32| pen.p(x, y);
        let push = |s: Shape| pen.push(s);
        let line = |a: (f32, f32), b: (f32, f32)| pen.line(a, b);

        match name {
            // An envelope: the flap is what makes it read as mail at 20px.
            "mail" => {
                pen.rect_outline(0.12, 0.24, 0.88, 0.76);
                push(Shape::line(
                    vec![p(0.12, 0.24), p(0.5, 0.54), p(0.88, 0.24)],
                    thin,
                ));
            }
            // The same, opened: a message that has been read.
            "mail_open" => {
                push(Shape::line(
                    vec![p(0.12, 0.78), p(0.12, 0.42), p(0.5, 0.18), p(0.88, 0.42), p(0.88, 0.78)],
                    thin,
                ));
                line((0.12, 0.78), (0.88, 0.78));
                push(Shape::line(
                    vec![p(0.12, 0.42), p(0.5, 0.66), p(0.88, 0.42)],
                    thin,
                ));
            }
            // Reply: an arrow turning back on itself, the way every mail
            // client has drawn it for thirty years.
            "reply" => {
                push(Shape::line(
                    vec![p(0.42, 0.20), p(0.14, 0.42), p(0.42, 0.64)],
                    thick,
                ));
                push(Shape::line(
                    vec![p(0.14, 0.42), p(0.60, 0.42), p(0.82, 0.56), p(0.86, 0.84)],
                    thin,
                ));
            }
            // Send: away and upward, with the head on the far end.
            "send" => {
                line((0.14, 0.84), (0.78, 0.26));
                pen.arrow_head(0.86, 0.18, 0.74, -0.67, 0.22);
            }
            // One person: the account rows and the accounts button.
            "account" => {
                pen.circle_stroke(p(0.5, 0.33), pen.side() * 0.17, thin);
                push(Shape::line(
                    vec![p(0.16, 0.84), p(0.22, 0.62), p(0.78, 0.62), p(0.84, 0.84)],
                    thin,
                ));
            }
            // Back: an arrow pointing left.
            "back" => {
                line((0.22, 0.5), (0.84, 0.5));
                push(Shape::line(vec![p(0.46, 0.26), p(0.20, 0.5), p(0.46, 0.74)], thick));
            }
            // Forward: Reply's arrow, turned the other way.
            "forward" => {
                push(Shape::line(
                    vec![p(0.58, 0.20), p(0.86, 0.42), p(0.58, 0.64)],
                    thick,
                ));
                push(Shape::line(
                    vec![p(0.86, 0.42), p(0.40, 0.42), p(0.18, 0.56), p(0.14, 0.84)],
                    thin,
                ));
            }
            // Close: a cross, for taking a chip or a file off.
            "close" => {
                line((0.26, 0.26), (0.74, 0.74));
                line((0.74, 0.26), (0.26, 0.74));
            }
            // Compose: a pencil, writing on a line.
            "compose" => {
                push(Shape::closed_line(
                    vec![p(0.16, 0.68), p(0.58, 0.16), p(0.76, 0.32), p(0.34, 0.84)],
                    thin,
                ));
                line((0.16, 0.68), (0.14, 0.86));
                line((0.14, 0.86), (0.34, 0.84));
                line((0.52, 0.86), (0.88, 0.86));
            }
            // Settings: a wheel with six teeth.
            "gear" => {
                let side = pen.side();
                pen.circle_stroke(p(0.5, 0.5), side * 0.12, thin);
                pen.circle_stroke(p(0.5, 0.5), side * 0.27, thin);
                for i in 0..6 {
                    let (sin, cos) = (std::f32::consts::TAU * i as f32 / 6.0).sin_cos();
                    line(
                        (0.5 + 0.27 * cos, 0.5 + 0.27 * sin),
                        (0.5 + 0.40 * cos, 0.5 + 0.40 * sin),
                    );
                }
            }
            // Inbox: a tray, with the dip where mail lands.
            "inbox" => {
                push(Shape::line(
                    vec![p(0.12, 0.56), p(0.24, 0.20), p(0.76, 0.20), p(0.88, 0.56)],
                    thin,
                ));
                push(Shape::closed_line(
                    vec![p(0.12, 0.56), p(0.12, 0.82), p(0.88, 0.82), p(0.88, 0.56)],
                    thin,
                ));
                push(Shape::line(
                    vec![p(0.12, 0.56), p(0.36, 0.56), p(0.42, 0.66), p(0.58, 0.66), p(0.64, 0.56), p(0.88, 0.56)],
                    thin,
                ));
            }
            // Starred: a five-pointed star.
            "star" => {
                let points: Vec<Pos2> = (0..10)
                    .map(|i| {
                        let r = if i % 2 == 0 { 0.40 } else { 0.17 };
                        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / 5.0;
                        p(0.5 + r * a.cos(), 0.54 + r * a.sin())
                    })
                    .collect();
                push(Shape::closed_line(points, thin));
            }
            // Important: Gmail's marker, a tag pointing right.
            "important" => {
                push(Shape::closed_line(
                    vec![p(0.12, 0.26), p(0.66, 0.26), p(0.88, 0.5), p(0.66, 0.74), p(0.12, 0.74), p(0.30, 0.5)],
                    thin,
                ));
            }
            // Drafts: a page with its corner turned.
            "draft" => {
                push(Shape::closed_line(
                    vec![p(0.22, 0.12), p(0.58, 0.12), p(0.80, 0.34), p(0.80, 0.88), p(0.22, 0.88)],
                    thin,
                ));
                push(Shape::line(vec![p(0.58, 0.12), p(0.58, 0.34), p(0.80, 0.34)], thin));
            }
            // Spam: a circle with a stroke through it.
            "spam" => {
                pen.circle_stroke(p(0.5, 0.5), pen.side() * 0.36, thin);
                line((0.25, 0.25), (0.75, 0.75));
            }
            // A label of the account's own: a tag with its hole.
            "label" => {
                push(Shape::closed_line(
                    vec![p(0.14, 0.14), p(0.50, 0.14), p(0.86, 0.50), p(0.50, 0.86), p(0.14, 0.50)],
                    thin,
                ));
                pen.circle_stroke(p(0.32, 0.32), pen.side() * 0.05, thin);
            }
            // Keys: a keyboard's case, two rows of keys and the space bar.
            "keyboard" => {
                pen.rect_outline(0.08, 0.18, 0.92, 0.82);
                for x in [0.25, 0.42, 0.58, 0.75] {
                    pen.circle_filled(p(x, 0.36), pen.side() * 0.045);
                }
                for x in [0.33, 0.5, 0.67] {
                    pen.circle_filled(p(x, 0.5), pen.side() * 0.045);
                }
                line((0.3, 0.66), (0.7, 0.66));
            }
            // More: three dots in a row, for what does not fit on the bar.
            "more" => {
                for x in [0.22, 0.5, 0.78] {
                    push(Shape::circle_filled(p(x, 0.5), pen.side() * 0.065, color));
                }
            }
            // Reload: an almost-closed circle with a head on the loose end.
            "refresh" => {
                let (cx, cy, r) = (0.5f32, 0.52f32, 0.32f32);
                let from = std::f32::consts::PI * -0.62;
                let sweep = std::f32::consts::PI * 1.72;
                let arc: Vec<Pos2> = (0..=32)
                    .map(|i| {
                        let a = from + sweep * i as f32 / 32.0;
                        p(cx + r * a.cos(), cy + r * a.sin())
                    })
                    .collect();
                push(Shape::line(arc, thin));
                // The head sits on the open end, pointing the way round.
                pen.arrow_head(cx + r * from.cos(), cy + r * from.sin() - 0.02, 0.55, -0.83, 0.20);
            }
            // Plus: connecting an account.
            "plus" => {
                push(Shape::line_segment([p(0.5, 0.18), p(0.5, 0.82)], thick));
                push(Shape::line_segment([p(0.18, 0.5), p(0.82, 0.5)], thick));
            }
            // Save: an arrow coming down into a tray.
            "download" => {
                line((0.5, 0.14), (0.5, 0.56));
                pen.arrow_head(0.5, 0.64, 0.0, 1.0, 0.2);
                push(Shape::line(
                    vec![p(0.16, 0.62), p(0.16, 0.84), p(0.84, 0.84), p(0.84, 0.62)],
                    thin,
                ));
            }
            // Save as: a page with an arrow leaving it — it goes somewhere
            // you choose.
            "save_as" => {
                push(Shape::line(
                    vec![
                        p(0.66, 0.86),
                        p(0.18, 0.86),
                        p(0.18, 0.14),
                        p(0.52, 0.14),
                        p(0.70, 0.32),
                        p(0.70, 0.48),
                    ],
                    thin,
                ));
                push(Shape::line(vec![p(0.52, 0.14), p(0.52, 0.32), p(0.70, 0.32)], thin));
                line((0.42, 0.67), (0.80, 0.67));
                pen.arrow_head(0.90, 0.67, 1.0, 0.0, 0.16);
            }
            // A filled dot: the unread marker on a row.
            "unread" => {
                pen.circle_filled(p(0.5, 0.5), pen.side() * 0.22);
            }
            _ => return None,
        }
    }
    Some(pen.into_shapes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Rect;

    /// Every name the window asks for draws something — a missing glyph is an
    /// empty box on screen, which is exactly what these exist to avoid.
    #[test]
    fn every_icon_the_window_uses_is_drawn() {
        let rect = Rect::from_min_size(Pos2::ZERO, eframe::egui::Vec2::splat(24.0));
        for name in [
            "mail", "mail_open", "account", "back", "refresh", "plus", "unread", "reply", "send",
            "download", "save_as", "compose", "gear", "more", "inbox", "star", "important",
            "draft", "spam", "label", "forward", "close", "keyboard",
            // These come from the shared set.
            "trash", "open",
        ] {
            let shapes = build(rect, name, Color32::WHITE)
                .or_else(|| omarchy_kit::icons::build(rect, name, Color32::WHITE));
            assert!(shapes.is_some_and(|s| !s.is_empty()), "no icon called {name}");
        }
        assert!(build(rect, "sendmail", Color32::WHITE).is_none());
    }
}
