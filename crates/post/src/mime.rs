//! The parts of mail that are pure functions: base64url, walking a Gmail
//! payload, turning an HTML body into text, and naming a file that does not
//! overwrite one already there.
//!
//! Nothing here touches the network, so it is all testable against fixtures —
//! which is the point: this is where mail is at its most hostile, and where a
//! mistake is quietest.

use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::Value;

/// Gmail sends bodies and attachments base64url, usually unpadded.
pub fn b64url(data: &str) -> Result<Vec<u8>, String> {
    let trimmed: String = data
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '=')
        .collect();
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(trimmed.as_bytes())
        .map_err(|e| format!("the message body is not base64url: {e}"))
}

/// One header of a Gmail payload, by name, case-insensitively.
pub fn header(payload: &Value, name: &str) -> Option<String> {
    payload
        .get("headers")?
        .as_array()?
        .iter()
        .find(|h| {
            h.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
        .and_then(|h| h.get("value").and_then(Value::as_str))
        .map(str::to_owned)
}

/// A message's readable parts: the plain text one if it has one, the HTML one
/// if it has that.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Body {
    pub text: Option<String>,
    pub html: Option<String>,
}

/// Walks a Gmail payload for the body, ignoring anything that is a file.
pub fn body(payload: &Value) -> Body {
    let mut found = Body::default();
    walk_body(payload, &mut found);
    found
}

fn walk_body(part: &Value, found: &mut Body) {
    if found.text.is_some() && found.html.is_some() {
        return;
    }
    // A part with a filename is an attachment, whatever its type says.
    let is_file = part
        .get("filename")
        .and_then(Value::as_str)
        .is_some_and(|f| !f.is_empty());
    let mime = part
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let data = part
        .pointer("/body/data")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty());

    if !is_file && let Some(data) = data {
        let decoded = b64url(data)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        if mime.starts_with("text/plain") && found.text.is_none() {
            found.text = Some(decoded);
        } else if mime.starts_with("text/html") && found.html.is_none() {
            found.html = Some(decoded);
        }
    }

    if let Some(parts) = part.get("parts").and_then(Value::as_array) {
        for child in parts {
            walk_body(child, found);
        }
    }
}

/// One attachment on a message, as Gmail describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attach {
    pub filename: String,
    pub id: String,
    pub mime: String,
    pub size: u64,
}

/// Every attachment on a payload, in the order they appear.
pub fn attachments(payload: &Value) -> Vec<Attach> {
    let mut found = Vec::new();
    walk_attachments(payload, &mut found);
    found
}

fn walk_attachments(part: &Value, found: &mut Vec<Attach>) {
    let filename = part
        .get("filename")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id = part
        .pointer("/body/attachmentId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !filename.is_empty() && !id.is_empty() {
        found.push(Attach {
            filename: filename.to_string(),
            id: id.to_string(),
            mime: part
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream")
                .to_string(),
            size: part
                .pointer("/body/size")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        });
    }
    if let Some(parts) = part.get("parts").and_then(Value::as_array) {
        for child in parts {
            walk_attachments(child, found);
        }
    }
}

/// An HTML body as something a person — or a model — can read.
///
/// Not a renderer: scripts and styles go entirely, tags go, the block-level
/// ones leave a line break behind, and the handful of entities that actually
/// show up become their characters.
pub fn strip_html(html: &str) -> String {
    /// Tags whose insides are not text at all.
    const DROPPED: [&str; 3] = ["script", "style", "head"];
    /// Tags that end a line of reading.
    const BLOCKS: [&str; 13] = [
        "br",
        "p",
        "div",
        "tr",
        "li",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "table",
        "blockquote",
    ];

    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < html.len() {
        let rest = &lower[i..];
        if let Some(block) = DROPPED.into_iter().find(|tag| opens(rest, tag)) {
            // Skip to the end of the closing tag, or to the end of the message
            // if it never closes — which mail does.
            match closes(rest, block) {
                Some(at) => match rest[at..].find('>') {
                    Some(gt) => i += at + gt + 1,
                    None => break,
                },
                None => break,
            }
            continue;
        }
        if rest.starts_with('<') {
            let Some(end) = rest.find('>') else { break };
            let inner = &rest[1..end];
            let closing = inner.starts_with('/');
            let name: String = inner
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            // Only the opening tag breaks the line, so </p><p> is one break and
            // not two.
            if !closing && BLOCKS.contains(&name.as_str()) {
                out.push('\n');
            }
            if !closing && (name == "td" || name == "th") {
                out.push(' ');
            }
            i += end + 1;
            continue;
        }
        let ch = html[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    tidy(&entities(&out))
}

/// True if `rest` starts with `<tag` and the tag name ends there — so `<head`
/// does not match `<header`.
fn opens(rest: &str, tag: &str) -> bool {
    let Some(after) = rest.strip_prefix('<').and_then(|r| r.strip_prefix(tag)) else {
        return false;
    };
    after.is_empty() || after.starts_with(['>', ' ', '\t', '\n', '\r', '/'])
}

/// Where `</tag` starts in `rest`, name boundary respected.
fn closes(rest: &str, tag: &str) -> Option<usize> {
    let needle = format!("</{tag}");
    let mut from = 0;
    while let Some(at) = rest[from..].find(&needle) {
        let at = from + at;
        let after = &rest[at + needle.len()..];
        if after.is_empty() || after.starts_with(['>', ' ', '\t', '\n', '\r', '/']) {
            return Some(at);
        }
        from = at + needle.len();
    }
    None
}

/// The entities worth knowing about; anything else stays as it was written.
pub(crate) fn entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        // Last, so a literal &amp;lt; does not become a tag.
        .replace("&amp;", "&")
}

/// Collapses the whitespace HTML leaves behind: runs of spaces, trailing
/// space, and any run of blank lines down to one.
fn tidy(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut squeezed = String::with_capacity(line.len());
        let mut space = false;
        for ch in line.chars() {
            if ch.is_whitespace() {
                space = true;
                continue;
            }
            if space && !squeezed.is_empty() {
                squeezed.push(' ');
            }
            space = false;
            squeezed.push(ch);
        }
        if squeezed.is_empty() && lines.last().is_some_and(|l| l.is_empty()) {
            continue;
        }
        lines.push(squeezed);
    }
    while lines.first().is_some_and(|l| l.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// Never hand back more than this much body: "read my whole mailbox" must not
/// be possible by accident.
pub const MAX_TEXT: usize = 20_000;

/// A body with the quoted message under it taken off — every line behind `>`,
/// and the attribution line that introduced them.
///
/// In a conversation the quote is the message above it, already on screen:
/// printing it again once per reply is the same words five times over.
pub fn unquote(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with('>') {
            // "On <date>, <someone> wrote:" belongs to the quote it opens.
            while lines
                .last()
                .is_some_and(|last| last.trim().is_empty() || last.trim_end().ends_with("wrote:"))
            {
                lines.pop();
            }
            continue;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    lines.join("\n").trim_end().to_string()
}

/// Caps a body, saying so where it was cut.
pub fn cap(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}\n… (truncated; the full message is in the mailbox)")
}

/// A path in `dir` for `filename` that is not already taken: `report.pdf`,
/// then `report (1).pdf`, then `report (2).pdf`.
pub fn unique_path(dir: &Path, filename: &str) -> PathBuf {
    let name = safe_name(filename);
    let first = dir.join(&name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = split_extension(&name);
    for n in 1..10_000 {
        let candidate = match ext {
            Some(ext) => dir.join(format!("{stem} ({n}).{ext}")),
            None => dir.join(format!("{stem} ({n})")),
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem} ({})", crate::store::now()))
}

/// A filename from a message is a name, never a path: an attachment called
/// `../../.bashrc` writes a file called `.bashrc` where it was asked to.
fn safe_name(filename: &str) -> String {
    let base = filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(filename)
        .trim();
    if base.is_empty() || base == "." || base == ".." {
        return "attachment".to_string();
    }
    base.to_string()
}

fn split_extension(name: &str) -> (&str, Option<&str>) {
    match name.rsplit_once('.') {
        // A dotfile is all stem.
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => (stem, Some(ext)),
        _ => (name, None),
    }
}

/// A Gmail `internalDate` (milliseconds) as `YYYY-MM-DD HH:MM` UTC — short,
/// sortable, and the same everywhere.
pub fn stamp(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rest / 3600,
        (rest % 3600) / 60
    )
}

/// Days since the epoch to a calendar date (Howard Hinnant's civil_from_days).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A multipart message as Gmail actually returns it: alternative text and
    /// HTML, plus a file.
    fn fixture() -> Value {
        json!({
            "id": "18f3a2c",
            "internalDate": "1757673180000",
            "labelIds": ["INBOX", "UNREAD"],
            "payload": {
                "mimeType": "multipart/mixed",
                "filename": "",
                "headers": [
                    {"name": "Delivered-To", "value": "ed@acme.com"},
                    {"name": "From", "value": "Ale <ale@acme.com>"},
                    {"name": "Subject", "value": "Re: the quote"},
                    {"name": "Date", "value": "Fri, 12 Sep 2025 10:33:00 +0200"}
                ],
                "parts": [
                    {
                        "mimeType": "multipart/alternative",
                        "filename": "",
                        "parts": [
                            {
                                "mimeType": "text/plain",
                                "filename": "",
                                // "Looks good — send it.\n"
                                "body": {"size": 24, "data": "TG9va3MgZ29vZCDigJQgc2VuZCBpdC4K"}
                            },
                            {
                                "mimeType": "text/html",
                                "filename": "",
                                // "<div><p>Looks good</p></div>"
                                "body": {"size": 28, "data": "PGRpdj48cD5Mb29rcyBnb29kPC9wPjwvZGl2Pg"}
                            }
                        ]
                    },
                    {
                        "mimeType": "application/pdf",
                        "filename": "quote.pdf",
                        "body": {"size": 8412, "attachmentId": "ANGjdJ_att_1"}
                    }
                ]
            }
        })
    }

    #[test]
    fn headers_come_back_whatever_their_case() {
        let payload = &fixture()["payload"];
        assert_eq!(header(payload, "From").as_deref(), Some("Ale <ale@acme.com>"));
        assert_eq!(header(payload, "subject").as_deref(), Some("Re: the quote"));
        assert!(header(payload, "Reply-To").is_none());
    }

    #[test]
    fn the_plain_part_is_decoded_and_the_file_is_not_the_body() {
        let found = body(&fixture()["payload"]);
        assert_eq!(found.text.as_deref(), Some("Looks good — send it.\n"));
        assert_eq!(found.html.as_deref(), Some("<div><p>Looks good</p></div>"));
    }

    /// A message with no multipart at all: the body sits on the payload.
    #[test]
    fn a_single_part_message_is_its_own_body() {
        let payload = json!({
            "mimeType": "text/plain",
            "filename": "",
            "body": {"size": 6, "data": "aGVsbG8K"}
        });
        assert_eq!(body(&payload).text.as_deref(), Some("hello\n"));
        assert!(body(&payload).html.is_none());
    }

    #[test]
    fn attachments_carry_the_id_the_fetch_needs() {
        let found = attachments(&fixture()["payload"]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].filename, "quote.pdf");
        assert_eq!(found[0].id, "ANGjdJ_att_1");
        assert_eq!(found[0].mime, "application/pdf");
        assert_eq!(found[0].size, 8412);
    }

    #[test]
    fn base64url_decodes_padded_and_not() {
        assert_eq!(b64url("aGVsbG8").expect("decoded"), b"hello");
        assert_eq!(b64url("aGVsbG8=").expect("decoded"), b"hello");
        // The url-safe alphabet: - and _ where + and / would be.
        assert_eq!(b64url("--_-").expect("decoded"), vec![251, 239, 254]);
        assert!(b64url("not base64 *").is_err());
    }

    #[test]
    fn html_becomes_readable_text_and_scripts_go_entirely() {
        let html = "<html><head><style>p{color:red}</style></head><body>\
                    <p>Hello <b>Ed</b> &amp; team</p>\
                    <script>steal()</script>\
                    <p>Second&nbsp;line</p></body></html>";
        assert_eq!(strip_html(html), "Hello Ed & team\nSecond line");
    }

    #[test]
    fn a_table_keeps_its_cells_apart() {
        let html = "<table><tr><td>Jan</td><td>12</td></tr><tr><td>Feb</td><td>9</td></tr></table>";
        assert_eq!(strip_html(html), "Jan 12\nFeb 9");
    }

    /// Mail is hostile input: an unclosed tag must not eat the loop.
    #[test]
    fn broken_html_does_not_hang_or_panic() {
        assert_eq!(strip_html("<p>text<"), "text");
        assert_eq!(strip_html("<script>never closed"), "");
        assert_eq!(strip_html(""), "");
        assert_eq!(strip_html("just words"), "just words");
        // Multi-byte characters survive the byte walk.
        assert_eq!(strip_html("<p>Smörgås — 100 €</p>"), "Smörgås — 100 €");
    }

    #[test]
    fn a_body_is_capped_where_it_is_cut() {
        let long = "x".repeat(50);
        let short = cap(&long, 10);
        assert!(short.starts_with(&"x".repeat(10)));
        assert!(short.contains("truncated"));
        assert_eq!(cap("brief", 10), "brief");
    }

    #[test]
    fn a_second_file_of_the_same_name_gets_a_suffix() {
        let dir = std::env::temp_dir()
            .join("post-tests")
            .join(format!("unique-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch dir");

        let first = unique_path(&dir, "report.pdf");
        assert_eq!(first, dir.join("report.pdf"));
        std::fs::write(&first, b"one").expect("write");

        let second = unique_path(&dir, "report.pdf");
        assert_eq!(second, dir.join("report (1).pdf"));
        std::fs::write(&second, b"two").expect("write");

        assert_eq!(unique_path(&dir, "report.pdf"), dir.join("report (2).pdf"));

        // No extension, and a dotfile, both keep their whole name as the stem.
        std::fs::write(dir.join("notes"), b"x").expect("write");
        assert_eq!(unique_path(&dir, "notes"), dir.join("notes (1)"));
        std::fs::write(dir.join(".bashrc"), b"x").expect("write");
        assert_eq!(unique_path(&dir, ".bashrc"), dir.join(".bashrc (1)"));

        // A filename from a stranger is a name, not a path.
        assert_eq!(
            unique_path(&dir, "../../etc/passwd"),
            dir.join("passwd"),
            "an attachment must not write outside the folder it was given"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_internal_date_becomes_a_sortable_stamp() {
        // 2025-09-12 10:33:00 UTC
        assert_eq!(stamp(1_757_673_180_000), "2025-09-12 10:33");
        assert_eq!(stamp(0), "1970-01-01 00:00");
        // A leap day, and the year boundary.
        assert_eq!(stamp(1_709_208_000_000), "2024-02-29 12:00");
        assert_eq!(stamp(1_735_689_599_000), "2024-12-31 23:59");
    }

    /// A reply in a conversation carries the message above it quoted under it.
    /// The conversation already shows that message, so the quote comes off.
    #[test]
    fn a_quote_comes_off_a_message_in_a_conversation() {
        let reply = "Looks good — send it.\n\n\
                     On 2025-09-12 10:33, Ale <ale@acme.com> wrote:\n\
                     > Can you send the quote?\n\
                     > /Ale";
        assert_eq!(unquote(reply), "Looks good — send it.");

        // Nothing quoted is nothing to take off.
        assert_eq!(unquote("Just this."), "Just this.");
        assert_eq!(unquote(""), "");
        // A message that is only a quote says nothing of its own.
        assert_eq!(unquote("> everything\n> he said"), "");
        // And the words after a quote are still the sender's own.
        assert_eq!(
            unquote("Above.\n> quoted\nBelow."),
            "Above.\nBelow."
        );
    }
}
