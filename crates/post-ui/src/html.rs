//! An open message's HTML body, drawn by Blitz: parsed, styled and laid out in
//! this process, painted on the CPU into tiles that the conversation scrolls
//! like anything else it shows. No web engine, no native view, no scripts.
//!
//! What it does is what the body needs: the page around the message (the
//! theme's colours, 14px text on 1.5 lines), pictures over https, http and
//! `data:` — pictures only: a stylesheet or a font from the network is never
//! applied — links handed to the browser, and selecting text for Copy.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyrender::{PaintScene as _, render_to_buffer};
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{DocumentConfig, EventDriver, Node, NodeId, NoopEventHandler, Point, QualName, local_name, ns};
use blitz_html::HtmlDocument;
use blitz_paint::paint_scene;
use blitz_traits::events::{
    BlitzPointerEvent, BlitzPointerId, MouseEventButton, MouseEventButtons, PointerCoords,
    PointerDetails, UiEvent,
};
use blitz_traits::navigation::{NavigationOptions, NavigationProvider};
use blitz_traits::net::{Body, Bytes, Method, NetHandler, NetProvider, Request, Url};
use blitz_traits::shell::{ColorScheme, Viewport};
use eframe::egui::{
    Color32, ColorImage, Context, CursorIcon, Rect, Response, Sense, TextureHandle,
    TextureOptions, Ui, Vec2, pos2,
};

/// The page mail is drawn on, as [`page`] sets it.
const PAPER: Color32 = Color32::WHITE;

/// How tall one painted tile is, in points.
const TILE: f32 = 512.0;
/// The most of a message that is rendered. Anything past this is not reading.
const MAX_HTML: usize = 4 * 1024 * 1024;
/// The most one picture may weigh.
const MAX_PICTURE: u64 = 10 * 1024 * 1024;

/// The body on screen, if one is: one message's document at a time.
#[derive(Default)]
pub struct Html {
    open: Option<Open>,
}

impl Html {
    /// Draws `page` — the message `key` — at the width the ui has and as tall
    /// as it is, and answers the pointer over it: a link opens in the browser,
    /// a drag selects text.
    pub fn show(&mut self, ui: &mut Ui, key: &str, page: &str, accent: Color32) {
        // The theme's accent, sunk into the page's white: a selection that
        // the text over it reads on.
        blitz_paint::set_selection_color(selection(accent, PAPER));
        let ctx = ui.ctx().clone();
        let width = ui.available_width().floor().max(1.0);
        let ppp = ctx.pixels_per_point();
        if self.open.as_ref().is_none_or(|open| open.key != key) {
            self.open = Some(Open::new(&ctx, key, page, width, ppp));
        }
        let open = self.open.as_mut().expect("a document");
        open.fit(width, ppp);
        open.take_pictures();
        let height = open.height();
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(width, height), Sense::click_and_drag());
        open.pointer(ui, rect, &response);
        // Ctrl+C reaches egui as a copy event, not as a key press, so the
        // keymap never sees it: the selection is copied here.
        if ui.input(|i| i.events.iter().any(|e| matches!(e, eframe::egui::Event::Copy)))
            && let Some(text) = open.doc.as_ref().get_selected_text()
        {
            ui.ctx().copy_text(text);
        }
        open.paint(ui, rect, PAPER);
    }

    /// The text selected in the body, for Copy.
    pub fn selected_text(&self) -> Option<String> {
        self.open.as_ref()?.doc.as_ref().get_selected_text()
    }

    /// No message is open: the document goes, and its pictures with it.
    pub fn close(&mut self) {
        self.open = None;
    }
}

/// One message's document, and the tiles painted from it so far.
struct Open {
    key: String,
    doc: HtmlDocument,
    /// What the document is laid out at: points wide, pixels per point.
    width: f32,
    ppp: f32,
    tiles: HashMap<u32, TextureHandle>,
    /// A picture arrived since the last frame.
    landed: Arc<AtomicBool>,
    /// The primary button went down over the body and is still down.
    dragging: bool,
    /// The node under the pointer, so a hover is told to the document once.
    hovered: Option<blitz_dom::NodeId>,
}

impl Open {
    fn new(ctx: &Context, key: &str, page: &str, width: f32, ppp: f32) -> Open {
        let landed = Arc::new(AtomicBool::new(false));
        let pictures = Arc::new(Pictures {
            landed: landed.clone(),
            ctx: ctx.clone(),
        });
        let mut doc = HtmlDocument::from_html(
            page,
            DocumentConfig {
                viewport: Some(viewport(width, ppp)),
                net_provider: Some(pictures),
                navigation_provider: Some(Arc::new(Browser)),
                ..Default::default()
            },
        );
        tables_as_browsers_lay_them_out(&mut doc);
        padded_inlines_as_boxes(&mut doc);
        doc.as_mut().resolve(0.0);
        Open {
            key: key.to_string(),
            doc,
            width,
            ppp,
            tiles: HashMap::new(),
            landed,
            dragging: false,
            hovered: None,
        }
    }

    /// Lays the document out again when the column or the screen changed.
    fn fit(&mut self, width: f32, ppp: f32) {
        if (width - self.width).abs() < 0.5 && ppp == self.ppp {
            return;
        }
        self.width = width;
        self.ppp = ppp;
        self.doc.as_mut().set_viewport(viewport(width, ppp));
        self.doc.as_mut().resolve(0.0);
        self.tiles.clear();
    }

    /// Takes in the pictures that arrived: they change the layout and the
    /// pixels, so every tile is painted again.
    fn take_pictures(&mut self) {
        if self.landed.swap(false, Ordering::SeqCst) {
            self.doc.as_mut().resolve(0.0);
            self.tiles.clear();
        }
    }

    /// The document's height, in points — CSS pixels are points here.
    fn height(&self) -> f32 {
        self.doc.as_ref().root_element().final_layout().size.height.max(1.0)
    }

    /// The pointer over the body, handed to the document in its own
    /// coordinates: a press and release on a link is a click it opens, a
    /// drag selects. The cursor says which one the pointer is over.
    fn pointer(&mut self, ui: &Ui, rect: Rect, response: &Response) {
        let Some(at) = response.interact_pointer_pos().or(response.hover_pos()) else {
            // Gone from the body: whatever it hovered is not hovered any more.
            if self.hovered.take().is_some() {
                self.send(UiEvent::PointerLeave(event(-1.0, -1.0, MouseEventButtons::None)));
            }
            return;
        };
        let (x, y) = (at.x - rect.min.x, at.y - rect.min.y);
        if response.hovered() {
            ui.ctx().set_cursor_icon(if self.over_link(x, y) {
                CursorIcon::PointingHand
            } else {
                CursorIcon::Text
            });
            // A hover that moved onto something else, for the page's :hover.
            let under = self.doc.as_ref().hit(x, y).map(|hit| hit.node_id);
            if !self.dragging && under != self.hovered {
                self.hovered = under;
                self.send(UiEvent::PointerMove(event(x, y, MouseEventButtons::None)));
            }
        }
        let (pressed, down, released, moved) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.primary_released(),
                i.pointer.delta() != Vec2::ZERO,
            )
        });
        if pressed && response.hovered() {
            self.send(UiEvent::PointerDown(event(x, y, MouseEventButtons::Primary)));
            self.dragging = true;
        } else if self.dragging && down && moved {
            self.send(UiEvent::PointerMove(event(x, y, MouseEventButtons::Primary)));
        }
        if self.dragging && released {
            self.send(UiEvent::PointerUp(event(x, y, MouseEventButtons::None)));
            self.dragging = false;
        }
    }

    /// One event into the document; the selection or a hover may have
    /// changed what it looks like, so it is styled again and the tiles are
    /// painted again.
    fn send(&mut self, event: UiEvent) {
        EventDriver::new(&mut self.doc, NoopEventHandler).handle_ui_event(event);
        self.doc.as_mut().resolve(0.0);
        self.tiles.clear();
    }

    /// True when the point is on a link.
    fn over_link(&self, x: f32, y: f32) -> bool {
        let doc = self.doc.as_ref();
        let Some(hit) = doc.hit(x, y) else {
            return false;
        };
        let mut id = Some(hit.node_id);
        while let Some(node) = id.and_then(|id| doc.get_node(id)) {
            if node.data.is_element_with_tag_name(&local_name!("a"))
                && node.attr(local_name!("href")).is_some()
            {
                return true;
            }
            id = node.parent;
        }
        false
    }

    /// Paints the tiles the conversation is showing, rendering any not yet
    /// painted.
    fn paint(&mut self, ui: &Ui, rect: Rect, background: Color32) {
        let seen = rect.intersect(ui.clip_rect());
        if seen.height() <= 0.0 || seen.width() <= 0.0 {
            return;
        }
        let first = ((seen.min.y - rect.min.y) / TILE).floor().max(0.0) as u32;
        let last = ((seen.max.y - rect.min.y) / TILE).floor().max(0.0) as u32;
        let painter = ui.painter_at(rect);
        for k in first..=last {
            let texture = match self.tiles.get(&k) {
                Some(texture) => texture.clone(),
                None => {
                    let texture = self.render(ui.ctx(), k, background);
                    self.tiles.insert(k, texture.clone());
                    texture
                }
            };
            let top = rect.min.y + k as f32 * TILE;
            painter.image(
                texture.id(),
                Rect::from_min_size(pos2(rect.min.x, top), Vec2::new(self.width, TILE)),
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }

    /// Tile `k`: the document scrolled to it, painted on the CPU.
    fn render(&mut self, ctx: &Context, k: u32, background: Color32) -> TextureHandle {
        let ppp = self.ppp;
        let (w, h) = (
            (self.width * ppp).round() as u32,
            (TILE * ppp).round() as u32,
        );
        let doc = self.doc.as_mut();
        doc.set_viewport_scroll(Point {
            x: 0.0,
            y: (k as f32 * TILE) as f64,
        });
        let ground = peniko::Color::from_rgb8(background.r(), background.g(), background.b());
        let pixels = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| {
                scene.fill(
                    peniko::Fill::NonZero,
                    Default::default(),
                    ground,
                    Default::default(),
                    &peniko::kurbo::Rect::new(0.0, 0.0, w as f64, h as f64),
                );
                paint_scene(scene, doc, ppp as f64, w, h, 0, 0);
            },
            w,
            h,
        );
        self.doc.as_mut().set_viewport_scroll(Point { x: 0.0, y: 0.0 });
        let image = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &pixels);
        ctx.load_texture(format!("post-body-{}-{k}", self.key), image, TextureOptions::LINEAR)
    }
}

/// The selection colour: the accent, 45% of the way over the background.
fn selection(accent: Color32, background: Color32) -> [u8; 4] {
    let mix = |a: u8, b: u8| (a as f32 * 0.45 + b as f32 * 0.55).round() as u8;
    [
        mix(accent.r(), background.r()),
        mix(accent.g(), background.g()),
        mix(accent.b(), background.b()),
        255,
    ]
}

/// What the document is laid out at: a tile-tall viewport of the column.
fn viewport(width: f32, ppp: f32) -> Viewport {
    Viewport::new(
        (width * ppp).round() as u32,
        (TILE * ppp).round() as u32,
        ppp,
        ColorScheme::Light,
    )
}

/// A pointer event at a point of the document, from the mouse.
fn event(x: f32, y: f32, buttons: MouseEventButtons) -> BlitzPointerEvent {
    BlitzPointerEvent {
        id: BlitzPointerId::Mouse,
        is_primary: true,
        coords: PointerCoords {
            page_x: x,
            page_y: y,
            screen_x: x,
            screen_y: y,
            client_x: x,
            client_y: y,
        },
        button: MouseEventButton::Main,
        buttons,
        mods: Default::default(),
        details: PointerDetails::default(),
        element: Default::default(),
        active_pointers: Default::default(),
    }
}

/// Fetches what the document asks for, and delivers only pictures.
struct Pictures {
    landed: Arc<AtomicBool>,
    ctx: Context,
}

impl NetProvider for Pictures {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let (landed, ctx) = (self.landed.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let url = request.url;
            let Some(bytes) = fetch(&url).filter(|bytes| picture(bytes)) else {
                return;
            };
            handler.bytes(url.to_string(), Bytes::from(bytes));
            landed.store(true, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }
}

/// The bytes behind a URL: over https or http, or out of a `data:` URL.
fn fetch(url: &Url) -> Option<Vec<u8>> {
    match url.scheme() {
        "https" | "http" => {
            let reply = ureq::get(url.as_str())
                .timeout(Duration::from_secs(20))
                .call()
                .ok()?;
            let mut bytes = Vec::new();
            reply
                .into_reader()
                .take(MAX_PICTURE)
                .read_to_end(&mut bytes)
                .ok()?;
            Some(bytes)
        }
        "data" => data_url::DataUrl::process(url.as_str())
            .ok()?
            .decode_to_vec()
            .ok()
            .map(|(bytes, _)| bytes),
        _ => None,
    }
}

/// True when the bytes are a PNG, JPEG, GIF or WebP picture.
fn picture(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG")
        || bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || bytes.starts_with(b"GIF8")
        || (bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
}

/// Links go to the browser — or, for mailto, to the mail app. A form posting
/// something is not a link, and goes nowhere.
struct Browser;

impl NavigationProvider for Browser {
    fn navigate_to(&self, options: NavigationOptions) {
        let link = options.method == Method::GET
            && matches!(options.document_resource, Body::Empty)
            && matches!(options.url.scheme(), "https" | "http" | "mailto");
        if link {
            crate::state::open_file(options.url.as_str());
        }
    }
}

/// The page a message's HTML is rendered in: black on white with a browser's
/// blue links, as a mail app shows mail — mail is written for that page, and
/// text a message leaves uncoloured on a card of its own stays readable. 14px
/// text at a browser's line height, 14px of padding, pictures and tables no
/// wider than the column. What a message sets for itself it keeps. Only the
/// selection takes the theme's accent.
pub fn page(html: &str, accent: [u8; 3]) -> String {
    let body = if html.len() > MAX_HTML {
        &html[..floor_char_boundary(html, MAX_HTML)]
    } else {
        html
    };
    let (before, after) = split_doctype(body);
    let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <style>html,body{{margin:0;padding:0;background:#fff;color:#000;\
         font:14px system-ui,sans-serif;overflow-wrap:break-word}}\
         body{{padding:14px}}\
         img{{max-width:100%;height:auto}} \
         table{{max-width:100%}} \
         ::selection{{background:{accent};color:#fff}}</style></head><body>{before}{after}</body></html>",
        accent = hex(accent),
    )
}

/// Tables as a browser lays them out, where Blitz would hide the message,
/// restated before the document is styled:
///
/// - A table's, a row's or a cell's `height` is a minimum: it grows to fit
///   what is in it (CSS 2.2 §17.5.3). Blitz takes it as a fixed size, and
///   mail leans on the difference — `height:0` on a cell, `height:100%` on
///   the table in it. Each is restated as the `min-height` Blitz honours.
/// - A cell set `display:block` — as mail sets its content cell, for phones —
///   stays in its row, in an anonymous cell (CSS 2.2 §17.2.1). Blitz leaves it
///   out of the table, and the message with it. The `display` is dropped.
/// - A cell's `max-width` is ignored by browsers. Blitz applies it, against a
///   column sized by content, and a column of `max-width:20%` shrinks to a
///   letter a line. The `max-width` is dropped.
fn tables_as_browsers_lay_them_out(doc: &mut HtmlDocument) {
    let tables = [local_name!("table"), local_name!("tr"), local_name!("td"), local_name!("th")];
    let mut changes: Vec<(NodeId, String, bool)> = Vec::new();
    let mut stack: Vec<&Node> = vec![doc.as_ref().root_node()];
    while let Some(node) = stack.pop() {
        if let Some(el) = node.element_data().filter(|el| tables.contains(&el.name.local)) {
            let attribute = node.attr(local_name!("height"));
            let style = node.attr(local_name!("style")).unwrap_or("");
            let is_cell = el.name.local == local_name!("td") || el.name.local == local_name!("th");
            let kept = if is_cell { cell_kept(style) } else { None };
            let heights = restated(kept.as_deref().unwrap_or(style), attribute);
            let drop_attribute = heights.is_some() && attribute.is_some();
            if let Some(style) = heights.or(kept) {
                changes.push((node.id, style, drop_attribute));
            }
        }
        stack.extend(node.children.iter().map(|&child| node.with(child)));
    }
    let mut mutator = doc.as_mut().mutate();
    let name = |local| QualName::new(None, ns!(), local);
    for (id, style, had_attribute) in changes {
        mutator.set_attribute(id, name(local_name!("style")), &style);
        if had_attribute {
            mutator.clear_attribute(id, name(local_name!("height")));
        }
    }
}

/// A link or a span with a background and padding — mail's buttons — as a
/// browser paints it: the background round the text and its padding, its
/// corners rounded. Blitz paints an inline element's background tight round
/// the text, and leaves no room for its padding. As an inline-block it gets
/// both; at the line height a browser's background is drawn at.
fn padded_inlines_as_boxes(doc: &mut HtmlDocument) {
    let inline = [local_name!("a"), local_name!("span")];
    let mut changes: Vec<(NodeId, String)> = Vec::new();
    let mut stack: Vec<&Node> = vec![doc.as_ref().root_node()];
    while let Some(node) = stack.pop() {
        if node.element_data().is_some_and(|el| inline.contains(&el.name.local))
            && let Some(style) = node.attr(local_name!("style")).and_then(as_box)
        {
            changes.push((node.id, style));
        }
        stack.extend(node.children.iter().map(|&child| node.with(child)));
    }
    let mut mutator = doc.as_mut().mutate();
    for (id, style) in changes {
        mutator.set_attribute(id, QualName::new(None, ns!(), local_name!("style")), &style);
    }
}

/// `style` made an inline-block's, when it pads a background and sets no
/// display of its own; `None` otherwise.
fn as_box(style: &str) -> Option<String> {
    let names: Vec<String> = style
        .split(';')
        .filter_map(|declaration| declaration.split_once(':'))
        .filter(|(_, value)| {
            let value = value.trim().to_ascii_lowercase();
            !value.is_empty() && value != "0" && value != "0px" && value != "none" && value != "transparent"
        })
        .map(|(name, _)| name.trim().to_ascii_lowercase())
        .collect();
    let padded = names.iter().any(|name| name.starts_with("padding"));
    let filled = names.iter().any(|name| name == "background" || name == "background-color");
    let displayed = style.to_ascii_lowercase().contains("display");
    (padded && filled && !displayed).then(|| {
        let style = style.trim_end().trim_end_matches(';');
        format!("{style};display:inline-block;line-height:normal")
    })
}

/// A cell's `style` as a browser reads it: without a `display` that takes it
/// out of its row — `block` or `inline-block` — and without a `max-width`.
/// `None` when it has neither; a `display:none` stays.
fn cell_kept(style: &str) -> Option<String> {
    let mut dropped = false;
    let kept: Vec<&str> = style
        .split(';')
        .filter(|declaration| {
            let out = declaration.split_once(':').is_some_and(|(name, value)| {
                let value: String = value.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
                let value = value.trim_end_matches("!important");
                let name = name.trim();
                (name.eq_ignore_ascii_case("display") && (value == "block" || value == "inline-block"))
                    || name.eq_ignore_ascii_case("max-width")
            });
            dropped |= out;
            !out
        })
        .collect();
    dropped.then(|| kept.join(";"))
}

/// `style` with its `height` made `min-height` — or, when it sets no height,
/// with the `height` attribute added as one, the attribute being the weaker
/// of the two. `None` when there is no height to restate.
fn restated(style: &str, attribute: Option<&str>) -> Option<String> {
    let mut found = false;
    let declarations: Vec<String> = style
        .split(';')
        .map(|declaration| match declaration.split_once(':') {
            Some((name, value)) if name.trim().eq_ignore_ascii_case("height") => {
                found = true;
                format!("min-height:{value}")
            }
            _ => declaration.to_string(),
        })
        .collect();
    let mut out = declarations.join(";");
    if found {
        return Some(out);
    }
    let height = attribute?.trim();
    let height = if !height.is_empty() && height.chars().all(|c| c.is_ascii_digit() || c == '.') {
        format!("{height}px")
    } else if height.ends_with('%') || height.ends_with("px") {
        height.to_string()
    } else {
        return None;
    };
    if !out.trim().is_empty() && !out.trim_end().ends_with(';') {
        out.push(';');
    }
    out.push_str(&format!("min-height:{height}"));
    Some(out)
}

/// The message either side of the `<!doctype>` it came with, so the page
/// around it declares the only one.
///
/// A saved message is usually a whole document, and a doctype is a thing a
/// document may state once, at the top. Nested in this page it is a token the
/// parser drops; it is dropped here instead, where it can be seen. Mail often
/// puts a comment before it, so the comments are stepped over to find it.
fn split_doctype(html: &str) -> (&str, &str) {
    let mut at = 0;
    loop {
        let rest = html[at..].trim_start();
        at = html.len() - rest.len();
        if rest.starts_with("<!--") {
            match rest.find("-->") {
                // Past this comment, and always forward: this cannot spin.
                Some(end) => at += end + "-->".len(),
                None => return (html, ""),
            }
            continue;
        }
        let doctype = rest
            .as_bytes()
            .get(.."<!doctype".len())
            .is_some_and(|start| start.eq_ignore_ascii_case(b"<!doctype"));
        return match doctype.then(|| rest.find('>')).flatten() {
            Some(end) => (&html[..at], &html[at + end + 1..]),
            None => (html, ""),
        };
    }
}

/// The largest index at or below `at` that does not split a character.
fn floor_char_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK: [u8; 3] = [95, 135, 190];

    /// The page is black on white, as a mail app shows mail, and the
    /// message's own markup is in it untouched.
    #[test]
    fn a_message_is_drawn_black_on_white() {
        let page = page("<p>hello <img src=\"https://acme.test/x.gif\"></p>", LINK);
        assert!(page.contains("<p>hello <img src=\"https://acme.test/x.gif\"></p>"));
        assert!(page.contains("background:#fff;color:#000"), "got {page}");
    }

    /// Text a message leaves uncoloured on a white card of its own is black,
    /// not the theme's light text.
    #[test]
    fn uncoloured_text_on_a_white_card_reads() {
        let message = "<div style=\"background:#ffffff\"><p id=\"hi\">Hi Eduárd,</p></div>";
        let mut doc = HtmlDocument::from_html(
            &page(message, LINK),
            DocumentConfig { viewport: Some(viewport(800.0, 1.0)), ..Default::default() },
        );
        doc.as_mut().resolve(0.0);
        let doc = doc.as_ref();
        let hi = doc.get_element_by_id("hi").and_then(|id| doc.get_node(id)).expect("the paragraph");
        let colour = hi.primary_styles().expect("styled").clone_color();
        let c = colour.components;
        assert_eq!((c.0, c.1, c.2), (0.0, 0.0, 0.0), "black: {colour:?}");
    }

    /// A saved message is a whole document — doctype, `<html>`, the lot — and
    /// the page it is put in stays one document all the same.
    #[test]
    fn a_whole_document_becomes_one_page() {
        for message in [
            "<!DOCTYPE html><html><body><p>a message</p></body></html>",
            "<!doctype html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \
             \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\">\
             <html><body><p>a message</p></body></html>",
            "<!-- saved --> \n<!doctype html><html><body><p>a message</p></body></html>",
            "<html><body><p>a message</p></body></html>",
        ] {
            let page = page(message, LINK);
            assert_eq!(page.to_ascii_lowercase().matches("<!doctype").count(), 1, "got {page}");
            assert!(page.starts_with("<!doctype html><html><head>"), "got {page}");
            assert!(page.ends_with("</body></html>"), "got {page}");
            assert!(page.contains("<p>a message</p>"), "got {page}");
        }
    }

    /// An enormous body is cut, and never mid-way through a character.
    #[test]
    fn an_enormous_body_is_cut_cleanly() {
        let huge = "é".repeat(MAX_HTML);
        let page = page(&huge, LINK);
        assert!(page.len() < huge.len() + 2048);
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.ends_with("</body></html>"));
    }

    /// A table's or a cell's height becomes a minimum; nothing else that
    /// says height is touched.
    #[test]
    fn heights_on_tables_and_cells_become_minimums() {
        assert_eq!(restated("width:580px;height:0", None).as_deref(), Some("width:580px;min-height:0"));
        assert_eq!(restated("HEIGHT: 100% ;", None).as_deref(), Some("min-height: 100% ;"));
        assert_eq!(restated("line-height:1;max-height:5px;min-height:2px", None), None);
        assert_eq!(restated("", Some("20")).as_deref(), Some("min-height:20px"));
        assert_eq!(restated("color:red", Some("50%")).as_deref(), Some("color:red;min-height:50%"));
        assert_eq!(restated("height:0", Some("20")).as_deref(), Some("min-height:0"), "the style outranks the attribute");
        assert_eq!(restated("", Some("auto")), None);
    }

    /// Mail as it is often built: a table restyled `display:block`, a cell
    /// in it of `height:0`, and in that a table of `height:100%`. Every line
    /// of what it holds still takes room.
    #[test]
    fn a_zero_height_cell_still_shows_what_it_holds() {
        let message = "<table style=\"display:block;width:100%\"><tbody><tr>\
            <th style=\"width:580px;height:0\"><table width=\"100%\" style=\"height:100%\"><tbody><tr>\
            <th><p>One</p><p>Two</p><p>Three</p><p>Four</p></th></tr></tbody></table></th></tr></tbody></table>";
        let mut doc = HtmlDocument::from_html(
            &page(message, LINK),
            DocumentConfig { viewport: Some(viewport(800.0, 1.0)), ..Default::default() },
        );
        tables_as_browsers_lay_them_out(&mut doc);
        doc.as_mut().resolve(0.0);
        let height = doc.as_ref().root_element().final_layout().size.height;
        assert!(height >= 4.0 * 14.0, "four lines take room: {height}");
    }

    /// Text that sets no line height of its own — mail often says `inherit` —
    /// gets a browser's, not one and a half lines.
    #[test]
    fn lines_are_spaced_as_a_browser_spaces_them() {
        let message = "<p style=\"margin:0;font-size:14px;line-height:inherit\">One<br>Two</p>";
        let mut doc = HtmlDocument::from_html(
            &page(message, LINK),
            DocumentConfig { viewport: Some(viewport(800.0, 1.0)), ..Default::default() },
        );
        doc.as_mut().resolve(0.0);
        let body = doc.as_ref().root_element().final_layout().size.height - 2.0 * 14.0;
        assert!(body < 2.0 * 1.45 * 14.0, "two lines, closer than 1.5 apart: {body}");
    }

    /// A cell set `display:block` loses that display, staying a cell; one set
    /// `display:none` stays hidden.
    #[test]
    fn a_cell_set_display_block_stays_a_cell() {
        let kept = cell_kept("display: block !important; width:100%").expect("dropped");
        assert!(!kept.contains("display") && kept.contains("width:100%"), "{kept}");
        assert!(cell_kept("DISPLAY:inline-block").is_some());
        assert_eq!(cell_kept("display:none"), None);
        assert_eq!(cell_kept("color:red"), None);
        let kept = cell_kept("width:20%;max-width:20%; Max-Width: 60%").expect("dropped");
        assert!(!kept.to_lowercase().contains("max-width") && kept.contains("width:20%"), "{kept}");
    }

    /// An order summary as shops send it: every cell `max-width:20%` and
    /// allowed to break anywhere. Its headers still read on one line.
    #[test]
    fn narrow_order_columns_keep_their_headers_on_a_line() {
        let cell = "word-break:break-all;line-height:22px;width:20%;max-width:20%";
        let message = format!(
            "<table style=\"width:100%\"><thead><tr>\
             <th style=\"{cell};width:60%;max-width:60%\">Description</th>\
             <th id=\"quantity\" style=\"{cell}\">Quantity</th><th style=\"{cell}\">Amount</th></tr></thead>\
             <tbody><tr><td style=\"{cell}\">Basic email renewal for 3 months</td>\
             <td style=\"{cell}\">1</td><td style=\"{cell}\">0.00 EUR</td></tr></tbody></table>"
        );
        let mut doc = HtmlDocument::from_html(
            &page(&message, LINK),
            DocumentConfig { viewport: Some(viewport(600.0, 1.0)), ..Default::default() },
        );
        tables_as_browsers_lay_them_out(&mut doc);
        doc.as_mut().resolve(0.0);
        let doc = doc.as_ref();
        let quantity = doc.get_element_by_id("quantity").and_then(|id| doc.get_node(id)).expect("the header");
        let height = quantity.final_layout().size.height;
        assert!(height < 2.0 * 22.0, "one line: {height}");
    }

    /// Mail as it is often built: its content in the middle cell of a row,
    /// set `display:block` for phones. What it holds takes room.
    #[test]
    fn a_content_cell_set_display_block_still_shows() {
        let message = "<table width=\"100%\"><tbody><tr><td></td>\
            <td style=\"display:block !important;max-width:600px !important;width:100% !important;margin:0 auto\">\
            <p>One</p><p>Two</p><p>Three</p></td><td></td></tr></tbody></table>";
        let mut doc = HtmlDocument::from_html(
            &page(message, LINK),
            DocumentConfig { viewport: Some(viewport(800.0, 1.0)), ..Default::default() },
        );
        tables_as_browsers_lay_them_out(&mut doc);
        doc.as_mut().resolve(0.0);
        let height = doc.as_ref().root_element().final_layout().size.height;
        assert!(height >= 3.0 * 14.0, "three lines take room: {height}");
    }

    /// A link with a background and padding becomes a box; one without either
    /// stays as it is, and so does one that sets its own display.
    #[test]
    fn a_padded_link_with_a_background_becomes_a_box() {
        let style = as_box("padding:20px;line-height:40px;background-color:#3ec1e6;").expect("a button");
        assert!(style.ends_with("display:inline-block;line-height:normal"), "{style}");
        assert_eq!(as_box("color:#06c;text-decoration:none"), None);
        assert_eq!(as_box("padding:4px"), None, "no background");
        assert_eq!(as_box("background:#eee;padding:0"), None, "no padding");
        assert_eq!(as_box("background:#eee;padding:8px;display:block"), None);
    }

    /// A mail button — a link padded round its text on a coloured background —
    /// is laid out as large as a browser paints it: its text and its padding.
    #[test]
    fn a_mail_button_takes_its_padding() {
        let message = "<p style=\"line-height:22px\"><a id=\"button\" href=\"https://acme.test\" \
            style=\"padding:20px;line-height:40px;font-size:14px;font-weight:bold;border-radius:5px;\
            color:#fff;text-decoration:none;background-color:#3ec1e6\">VIEW YOUR ORDERS</a></p>";
        let mut doc = HtmlDocument::from_html(
            &page(message, LINK),
            DocumentConfig { viewport: Some(viewport(600.0, 1.0)), ..Default::default() },
        );
        padded_inlines_as_boxes(&mut doc);
        doc.as_mut().resolve(0.0);
        let doc = doc.as_ref();
        let button = doc.get_element_by_id("button").and_then(|id| doc.get_node(id)).expect("the button");
        let size = button.final_layout().size;
        assert!(size.height > 50.0 && size.height < 70.0, "its text and 20 points above and below: {size:?}");
        assert!(size.width > 150.0, "its text and 20 points either side: {size:?}");
    }

    /// Only pictures are delivered: a stylesheet or a font is not one.
    #[test]
    fn only_pictures_are_taken() {
        assert!(picture(b"\x89PNG\r\n\x1a\n...."));
        assert!(picture(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(picture(b"GIF89a"));
        assert!(picture(b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!picture(b"body { color: red }"));
        assert!(!picture(b"wOF2\0\0"));
    }
}
