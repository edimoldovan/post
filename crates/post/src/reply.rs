//! Replying to a message: the draft, and sending it.
//!
//! Two steps, never one. [`draft`] builds the reply from the message being
//! answered — who it goes to, what it is about, which thread it belongs on —
//! and writes it down; [`send`] is the only thing here that puts mail on the
//! wire, and it sends a draft that already exists rather than words handed to
//! it. That is what lets the confirmation dialog show the whole message before
//! anything leaves this computer: by then it is written.
//!
//! The headers are the pure part and the interesting one. A reply goes to the
//! sender's `Reply-To` when they named one, carries `In-Reply-To` and a
//! `References` chain so every mail client threads it, and says `Re:` once
//! however many times the subject has been round already.
//!
//! A reply answers everybody: the sender in `To`, and everyone the message was
//! addressed to or copied in `Cc`, less the account replying and less anyone
//! named twice. Both lists are the reader's to change — what the fields say
//! when Send is pressed is what leaves.

use base64::Engine;
use serde_json::{json, Value};

use crate::gmail;

/// A reply, written and not yet sent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    /// What [`send`] is given to find this again.
    pub id: String,
    pub account: String,
    /// The message being answered.
    pub replying_to: String,
    /// Bare addresses: a display name is the sender's, not ours to repeat.
    pub to: Vec<String>,
    /// Everyone else on the message being answered.
    pub cc: Vec<String>,
    /// Copied without the others seeing it. Gmail takes it off what leaves.
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    /// The original's `Message-ID`, empty when it had none.
    pub in_reply_to: String,
    /// The chain the original carried, with its own id on the end.
    pub references: String,
    /// Gmail's thread, so the reply lands in the conversation.
    pub thread_id: String,
    /// Absolute paths of files that leave with the reply.
    pub attachments: Vec<String>,
}

impl Draft {
    pub fn to_json(&self) -> Value {
        json!({
            "draft_id": self.id,
            "account": self.account,
            "replying_to": self.replying_to,
            "to": address_line(&self.to),
            "cc": address_line(&self.cc),
            "bcc": address_line(&self.bcc),
            "subject": self.subject,
            "body": self.body,
            "in_reply_to": self.in_reply_to,
            "references": self.references,
            "thread_id": self.thread_id,
            "attachments": self.attachments,
            "sent": false,
        })
    }
}

// ------------------------------------------------------------- the pure parts

/// The bare address in a `From` or `Reply-To` header: `Ale <ale@acme.com>`
/// is `ale@acme.com`. A header that is only an address is itself.
pub fn address_of(header: &str) -> String {
    match (header.rfind('<'), header.rfind('>')) {
        (Some(open), Some(close)) if close > open + 1 => header[open + 1..close].trim().to_string(),
        _ => header.trim().to_string(),
    }
}

/// The header a reply answers: the sender's `Reply-To` where they named one,
/// and their `From` otherwise.
fn answering(from: &str, reply_to: Option<&str>) -> String {
    let named = reply_to.unwrap_or_default().trim();
    if named.is_empty() {
        from.trim().to_string()
    } else {
        named.to_string()
    }
}

/// Where a reply goes: the address the sender asked to be answered at, and the
/// one they wrote from otherwise.
pub fn recipient(from: &str, reply_to: Option<&str>) -> String {
    address_of(&answering(from, reply_to))
}

/// The bare addresses in one header, which may name several: a comma inside
/// quotes or angle brackets is part of a name, not a separator. Empty entries
/// are dropped — a trailing comma is a typo, not an address.
pub fn addresses(header: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut part = String::new();
    let (mut quoted, mut angled) = (false, false);
    let done = |part: &mut String, out: &mut Vec<String>| {
        let address = address_of(part);
        if !address.is_empty() {
            out.push(address);
        }
        part.clear();
    };
    for c in header.chars() {
        match c {
            '"' => quoted = !quoted,
            '<' if !quoted => angled = true,
            '>' if !quoted => angled = false,
            ',' if !quoted && !angled => {
                done(&mut part, &mut out);
                continue;
            }
            _ => {}
        }
        part.push(c);
    }
    done(&mut part, &mut out);
    out
}

/// A list of addresses as one field: what the compose view shows, and what it
/// is read back from.
pub fn address_line(addresses: &[String]) -> String {
    addresses.join(", ")
}

/// Everyone a reply answers: the sender in `To`, and everyone the message was
/// addressed to or copied in `Cc`. The account replying is not written to
/// itself, and nobody is named twice.
///
/// A message with only this account on it — a note to oneself, a list that
/// rewrote the sender — would leave nobody in `To`, so the first of the copies
/// moves up rather than leaving a reply that cannot go.
pub fn reply_all(
    from: &str,
    reply_to: Option<&str>,
    to: &str,
    cc: &str,
    own: &str,
) -> (Vec<String>, Vec<String>) {
    let own = own.trim().to_lowercase();
    let mut seen: Vec<String> = Vec::new();
    let mut keep = |header: &str, out: &mut Vec<String>| {
        for address in addresses(header) {
            let lower = address.to_lowercase();
            if lower == own || seen.contains(&lower) {
                continue;
            }
            seen.push(lower);
            out.push(address);
        }
    };
    let (mut senders, mut copies) = (Vec::new(), Vec::new());
    keep(&answering(from, reply_to), &mut senders);
    keep(to, &mut copies);
    keep(cc, &mut copies);
    if senders.is_empty() && !copies.is_empty() {
        senders.push(copies.remove(0));
    }
    (senders, copies)
}

/// `Re:` once. A subject that already says it is not made to say it twice,
/// however many clients have been round it.
pub fn re_subject(subject: &str) -> String {
    let subject = subject.trim();
    if subject.is_empty() {
        return "Re:".to_string();
    }
    if subject.len() >= 3 && subject[..3].eq_ignore_ascii_case("re:") {
        return subject.to_string();
    }
    format!("Re: {subject}")
}

/// `Fwd:` once, like `Re:`: a subject that already says it — or `Fw:` — is
/// not made to say it twice.
pub fn fwd_subject(subject: &str) -> String {
    let subject = subject.trim();
    let lower = subject.to_ascii_lowercase();
    if lower.starts_with("fwd:") || lower.starts_with("fw:") {
        return subject.to_string();
    }
    if subject.is_empty() {
        return "Fwd:".to_string();
    }
    format!("Fwd: {subject}")
}

/// The header a forwarded message goes under: who wrote it, when, about what,
/// and to whom — the lines every mail client writes above it. Pure: `original`
/// is the message as `gmail::original` fetches it.
pub fn forwarded_header(original: &Value) -> String {
    let payload = original.get("payload").unwrap_or(&Value::Null);
    let header = |name: &str| crate::mime::header(payload, name).unwrap_or_default();
    let date = crate::gmail::summary(original)
        .get("date")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut lines = vec!["---------- Forwarded message ---------".to_string()];
    for (name, value) in [
        ("From", header("From")),
        ("Date", date),
        ("Subject", header("Subject")),
        ("To", header("To")),
        ("Cc", header("Cc")),
    ] {
        if !value.trim().is_empty() {
            lines.push(format!("{name}: {}", value.trim()));
        }
    }
    lines.join("\n")
}

/// The `References` chain: what the original carried, then the original's own
/// `Message-ID` on the end. Whitespace is one space, and an id already in the
/// chain is not repeated.
pub fn references(existing: Option<&str>, message_id: &str) -> String {
    let mut chain: Vec<&str> = existing
        .unwrap_or_default()
        .split_whitespace()
        .filter(|id| !id.is_empty())
        .collect();
    let message_id = message_id.trim();
    if !message_id.is_empty() && !chain.contains(&message_id) {
        chain.push(message_id);
    }
    chain.join(" ")
}

/// The line above a quote: `On 2025-09-12 12:33, Ale <ale@acme.com> wrote:`.
/// The date and the sender as the message itself gives them — the same two
/// things the detail header shows.
pub fn attribution(date: &str, from: &str) -> String {
    match (date.trim(), from.trim()) {
        ("", "") => "The message being answered:".to_string(),
        ("", who) => format!("{who} wrote:"),
        (when, "") => format!("On {when}, the sender wrote:"),
        (when, who) => format!("On {when}, {who} wrote:"),
    }
}

/// The body that leaves: what was typed, then the message being answered,
/// line by line behind `> `.
///
/// Nothing is rewrapped — the original's lines are its own, and a quote that
/// refolded them would be quoting something nobody wrote. A message with no
/// text to quote leaves the reply as it was typed.
pub fn quoted_body(typed: &str, original_text: &str, attribution: &str) -> String {
    let quoted: Vec<String> = original_text
        .trim_end_matches('\n')
        .lines()
        .map(|line| format!("> {line}"))
        .collect();
    if quoted.is_empty() {
        return typed.to_string();
    }
    let quote = format!("{attribution}\n{}", quoted.join("\n"));
    let typed = typed.trim_end();
    if typed.is_empty() {
        return quote;
    }
    format!("{typed}\n\n{quote}")
}

/// The attribution line for one message, out of the JSON Gmail answered with.
pub fn attribution_of(original: &Value) -> String {
    let payload = original.get("payload").unwrap_or(&Value::Null);
    let from = crate::mime::header(payload, "From").unwrap_or_default();
    // The same field, from the same place, as the row and the detail header.
    let date = crate::gmail::summary(original)
        .get("date")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    attribution(&date, &from)
}

/// A header value a mail server will carry: itself when it is ASCII, and
/// RFC 2047 base64 otherwise — a Swedish subject is not 7-bit.
pub fn header_value(text: &str) -> String {
    if text.is_ascii() {
        return text.to_string();
    }
    format!(
        "=?UTF-8?B?{}?=",
        base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
    )
}

/// The whole reply as RFC 2822 text, ready to be base64url'd onto the wire.
///
/// The body goes base64 rather than as it stands: a mail body is UTF-8, has
/// lines of whatever length the writer felt like, and may say `.` on one of
/// its own. Encoded, none of that is anyone's problem.
pub fn raw_message(draft: &Draft) -> Result<String, String> {
    let mut head = vec![format!("To: {}", address_line(&draft.to))];
    if !draft.cc.is_empty() {
        head.push(format!("Cc: {}", address_line(&draft.cc)));
    }
    if !draft.bcc.is_empty() {
        head.push(format!("Bcc: {}", address_line(&draft.bcc)));
    }
    head.push(format!("Subject: {}", header_value(&draft.subject)));
    if !draft.in_reply_to.is_empty() {
        head.push(format!("In-Reply-To: {}", draft.in_reply_to));
    }
    if !draft.references.is_empty() {
        head.push(format!("References: {}", draft.references));
    }
    head.push("MIME-Version: 1.0".to_string());
    let text = wrapped(&base64::engine::general_purpose::STANDARD.encode(draft.body.as_bytes()));
    if draft.attachments.is_empty() {
        head.push("Content-Type: text/plain; charset=\"UTF-8\"".to_string());
        head.push("Content-Transfer-Encoding: base64".to_string());
        return Ok(format!("{}\r\n\r\n{}\r\n", head.join("\r\n"), text));
    }
    // With files aboard the reply is multipart/mixed: the text first, then
    // each file base64'd under its own name.
    let boundary = "post-part";
    head.push(format!(
        "Content-Type: multipart/mixed; boundary=\"{boundary}\""
    ));
    let mut parts = format!(
        "--{boundary}\r\nContent-Type: text/plain; charset=\"UTF-8\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n{text}\r\n"
    );
    for path in &draft.attachments {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("could not read the attachment {path}: {e}"))?;
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        parts.push_str(&format!(
            "--{boundary}\r\nContent-Type: application/octet-stream; name=\"{name}\"\r\n\
             Content-Disposition: attachment; filename=\"{name}\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n{}\r\n",
            wrapped(&base64::engine::general_purpose::STANDARD.encode(&bytes))
        ));
    }
    parts.push_str(&format!("--{boundary}--\r\n"));
    Ok(format!("{}\r\n\r\n{parts}", head.join("\r\n")))
}

/// base64 in lines a mail server will not fold for us.
fn wrapped(encoded: &str) -> String {
    encoded
        .as_bytes()
        .chunks(76)
        .map(|line| String::from_utf8_lossy(line).into_owned())
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// The message being answered, as the reply to it. Pure: `original` is the
/// JSON Gmail answered with, `original_text` its body as text, and nothing
/// here goes near the network.
///
/// The draft's body is the whole of what would leave: what was typed, and the
/// message it answers quoted under it. Both doors build a draft through here,
/// so the dialog that asks about sending shows exactly that.
pub fn draft_of(
    account: &str,
    id: &str,
    body: &str,
    original: &Value,
    original_text: &str,
) -> Draft {
    let payload = original.get("payload").unwrap_or(&Value::Null);
    let header = |name: &str| crate::mime::header(payload, name);
    let from = header("From").unwrap_or_default();
    let message_id = header("Message-ID")
        .or_else(|| header("Message-Id"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let (to, cc) = reply_all(
        &from,
        header("Reply-To").as_deref(),
        &header("To").unwrap_or_default(),
        &header("Cc").unwrap_or_default(),
        account,
    );
    Draft {
        id: String::new(),
        account: account.to_string(),
        replying_to: id.trim().to_string(),
        to,
        cc,
        bcc: Vec::new(),
        subject: re_subject(&header("Subject").unwrap_or_default()),
        body: quoted_body(body, original_text, &attribution_of(original)),
        references: references(header("References").as_deref(), &message_id),
        in_reply_to: message_id,
        attachments: Vec::new(),
        thread_id: original
            .get("threadId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

// ------------------------------------------------------------- Gmail's drafts

/// Writes the draft into the account's Drafts in Gmail — over its own id when
/// it has one — and keeps the id Gmail answers with.
fn write(draft: &mut Draft) -> Result<(), String> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(raw_message(draft)?.as_bytes());
    let id = (!draft.id.is_empty()).then_some(draft.id.as_str());
    let saved = gmail::draft_save(&draft.account, id, &raw, &draft.thread_id)?;
    draft.id = saved
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if let Some(thread) = saved
        .get("message")
        .and_then(|m| m.get("threadId"))
        .and_then(Value::as_str)
    {
        draft.thread_id = thread.to_string();
    }
    Ok(())
}

/// A draft's fields out of its message as Gmail holds it, in full format.
/// Pure. The attachments are their names: [`load`] is what fetches them.
pub fn draft_of_gmail(account: &str, draft_id: &str, message: &Value) -> Draft {
    let payload = message.get("payload").unwrap_or(&Value::Null);
    let header = |name: &str| crate::mime::header(payload, name).unwrap_or_default();
    Draft {
        id: draft_id.to_string(),
        account: account.to_string(),
        replying_to: String::new(),
        to: addresses(&header("To")),
        cc: addresses(&header("Cc")),
        bcc: addresses(&header("Bcc")),
        subject: header("Subject"),
        body: crate::mime::body(payload).text.unwrap_or_default(),
        in_reply_to: header("In-Reply-To"),
        references: header("References"),
        thread_id: message
            .get("threadId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        attachments: crate::mime::attachments(payload)
            .into_iter()
            .map(|file| file.filename)
            .collect(),
    }
}

/// One draft as Gmail has it, to show or to send: its files by name only.
pub fn peek(account: &str, draft_id: &str) -> Result<Draft, String> {
    let answer = gmail::draft_get(account, draft_id)?;
    let message = answer.get("message").cloned().unwrap_or_else(|| json!({}));
    Ok(draft_of_gmail(account, draft_id, &message))
}

/// One draft to write on: its files fetched to disk, so writing it again
/// carries them.
pub fn load(account: &str, draft_id: &str) -> Result<Draft, String> {
    let answer = gmail::draft_get(account, draft_id)?;
    let message = answer.get("message").cloned().unwrap_or_else(|| json!({}));
    let mut draft = draft_of_gmail(account, draft_id, &message);
    let message_id = message.get("id").and_then(Value::as_str).unwrap_or_default();
    let mut paths = Vec::with_capacity(draft.attachments.len());
    for name in &draft.attachments {
        let saved = gmail::attachment(account, message_id, Some(name))?;
        paths.push(
            saved
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        );
    }
    draft.attachments = paths;
    Ok(draft)
}

/// What is wrong with the addresses a draft is written to, if anything. A
/// draft may go to nobody yet; [`send`] is what asks for somebody.
fn misaddressed(draft: &Draft) -> Option<String> {
    draft
        .to
        .iter()
        .chain(&draft.cc)
        .chain(&draft.bcc)
        .find(|address| !address.contains('@'))
        .map(|address| format!("\"{address}\" is not an email address."))
}

/// True when there is nothing in it worth a draft in Gmail.
fn empty(draft: &Draft, typed: &str) -> bool {
    typed.trim().is_empty() && draft.attachments.is_empty()
}

// --------------------------------------------------------------- the verbs

/// Builds the reply to one message and writes it into Gmail's Drafts. Nothing
/// is sent.
///
/// `to`, `cc` and `bcc` are the reader's own lists, comma separated, where
/// they have changed what the reply-all came out as; leaving them out keeps
/// it. `draft_id` is the draft to write over, so a reply saved as it is typed
/// stays one draft.
///
/// A reply with nothing typed in it is not written down: the window asks for
/// one to read the headers and the quote off before a word has been typed, and
/// pressing Reply leaves nothing in Drafts.
///
/// The answer carries the message being answered as well as the reply: the
/// attribution line and the original's own text, unquoted, which is what the
/// compose view shows under the editor.
#[allow(clippy::too_many_arguments)]
pub fn draft(
    account: &str,
    id: &str,
    body: &str,
    to: Option<&str>,
    cc: Option<&str>,
    bcc: Option<&str>,
    attach: &[String],
    draft_id: Option<&str>,
) -> Result<Value, String> {
    let original = gmail::original(account, id)?;
    let text = original_text(account, id);
    let mut draft = draft_of(account, id, body, &original, &text);
    if let Some(to) = to {
        draft.to = addresses(to);
    }
    if let Some(cc) = cc {
        draft.cc = addresses(cc);
    }
    if let Some(bcc) = bcc {
        draft.bcc = addresses(bcc);
    }
    if let Some(fault) = misaddressed(&draft) {
        return Err(fault);
    }
    for path in attach {
        if !std::path::Path::new(path).is_file() {
            return Err(format!("there is no file at {path}"));
        }
    }
    draft.attachments = attach.to_vec();
    draft.id = draft_id.unwrap_or_default().trim().to_string();
    if !(draft.id.is_empty() && empty(&draft, body)) {
        write(&mut draft)?;
    }
    let mut answer = draft.to_json();
    answer["attribution"] = json!(attribution_of(&original));
    answer["original_text"] = json!(text);
    Ok(answer)
}

/// Forwards one message: a Gmail draft with the note typed above the message
/// itself, under its Forwarded message header, and the message's own files.
/// Nothing is sent.
///
/// `attach` left out is the forwarded message's files, fetched to disk; given,
/// it is exactly the files that go — the reader may have taken some off.
/// `draft_id` is the draft to write over. A forward with nothing typed and
/// nobody to go to is not written: the window asks for one to read the header
/// and the files off before a word has been typed.
///
/// It stays on the message's conversation, the way Gmail keeps a forward with
/// what it forwards.
#[allow(clippy::too_many_arguments)]
pub fn forward(
    account: &str,
    id: &str,
    body: &str,
    subject: Option<&str>,
    to: Option<&str>,
    cc: Option<&str>,
    bcc: Option<&str>,
    attach: Option<&[String]>,
    draft_id: Option<&str>,
) -> Result<Value, String> {
    let original = gmail::original(account, id)?;
    let text = original_text(account, id);
    let header = forwarded_header(&original);
    let payload = original.get("payload").unwrap_or(&Value::Null);
    let message_id = crate::mime::header(payload, "Message-ID")
        .or_else(|| crate::mime::header(payload, "Message-Id"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let attachments = match attach {
        Some(files) => {
            for path in files {
                if !std::path::Path::new(path).is_file() {
                    return Err(format!("there is no file at {path}"));
                }
            }
            files.to_vec()
        }
        None => forwarded_files(account, id)?,
    };
    let note = body.trim_end();
    let mut draft = Draft {
        id: draft_id.unwrap_or_default().trim().to_string(),
        account: account.to_string(),
        to: addresses(to.unwrap_or_default()),
        cc: addresses(cc.unwrap_or_default()),
        bcc: addresses(bcc.unwrap_or_default()),
        subject: subject.map_or_else(
            || fwd_subject(&crate::mime::header(payload, "Subject").unwrap_or_default()),
            str::to_string,
        ),
        body: match note {
            "" => format!("{header}\n\n{text}"),
            note => format!("{note}\n\n{header}\n\n{text}"),
        },
        references: references(crate::mime::header(payload, "References").as_deref(), &message_id),
        in_reply_to: message_id,
        thread_id: original
            .get("threadId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        attachments,
        ..Draft::default()
    };
    if let Some(fault) = misaddressed(&draft) {
        return Err(fault);
    }
    let typed = format!(
        "{body}{}{}{}",
        to.unwrap_or_default(),
        cc.unwrap_or_default(),
        bcc.unwrap_or_default()
    );
    if !(draft.id.is_empty() && typed.trim().is_empty()) {
        write(&mut draft)?;
    }
    let mut answer = draft.to_json();
    answer["attribution"] = json!(header);
    answer["original_text"] = json!(text);
    Ok(answer)
}

/// A message's own files, fetched to disk to go with a forward of it.
fn forwarded_files(account: &str, id: &str) -> Result<Vec<String>, String> {
    let names: Vec<String> = gmail::read(account, id)?
        .get("attachments")
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(|file| {
                    file.get("name")
                        .or_else(|| file.get("filename"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    names
        .iter()
        .map(|name| {
            gmail::attachment(account, id, Some(name)).map(|saved| {
                saved
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
        })
        .collect()
}

/// Writes a new message into Gmail's Drafts — not a reply: no thread, nothing
/// quoted. Nothing is sent.
///
/// `draft_id` is the draft to write over. One that was a reply stays one:
/// the thread and the headers that thread it are kept, so a reply saved from
/// the reply screen and opened again later is still that reply.
#[allow(clippy::too_many_arguments)]
pub fn new_message(
    account: &str,
    to: &str,
    cc: &str,
    bcc: &str,
    subject: &str,
    body: &str,
    attach: &[String],
    draft_id: Option<&str>,
) -> Result<Value, String> {
    let mut draft = Draft {
        id: draft_id.unwrap_or_default().trim().to_string(),
        account: account.to_string(),
        to: addresses(to),
        cc: addresses(cc),
        bcc: addresses(bcc),
        subject: subject.to_string(),
        body: body.to_string(),
        attachments: attach.to_vec(),
        ..Draft::default()
    };
    if let Some(fault) = misaddressed(&draft) {
        return Err(fault);
    }
    for path in attach {
        if !std::path::Path::new(path).is_file() {
            return Err(format!("there is no file at {path}"));
        }
    }
    if !draft.id.is_empty() {
        let before = peek(account, &draft.id)?;
        draft.in_reply_to = before.in_reply_to;
        draft.references = before.references;
        draft.thread_id = before.thread_id;
    }
    let typed = format!("{subject}{body}{to}{cc}{bcc}");
    if !(draft.id.is_empty() && empty(&draft, &typed)) {
        write(&mut draft)?;
    }
    Ok(draft.to_json())
}

/// The text of the message being answered: the read cache when it is there,
/// and a fetch otherwise. A body that could not be had is no quote rather than
/// a failed reply — the words typed still go.
fn original_text(account: &str, id: &str) -> String {
    gmail::read(account, id)
        .ok()
        .and_then(|message| {
            message
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// Sends a draft as Gmail holds it, on the thread it is a reply to.
pub fn send(account: &str, draft_id: &str) -> Result<Value, String> {
    let draft = peek(account, draft_id)?;
    if draft.to.is_empty() {
        return Err("This draft goes to nobody yet: put an address in To.".to_string());
    }
    let answer = gmail::draft_send(account, draft_id)?;
    Ok(json!({
        "account": account,
        "draft_id": draft_id,
        "to": address_line(&draft.to),
        "cc": address_line(&draft.cc),
        "subject": draft.subject,
        "id": answer.get("id").and_then(Value::as_str).unwrap_or_default(),
        "thread_id": answer
            .get("threadId")
            .and_then(Value::as_str)
            .unwrap_or(draft.thread_id.as_str()),
        "sent": true,
    }))
}

/// Deletes a draft from Gmail for good.
pub fn discard(account: &str, draft_id: &str) -> Result<Value, String> {
    gmail::draft_delete(account, draft_id)?;
    Ok(json!({"account": account, "draft_id": draft_id, "discarded": true}))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message in metadata format, exactly as `gmail::original` fetches one.
    fn original() -> Value {
        json!({
            "id": "18f3a2c9b1",
            "threadId": "18f3a2c9b0",
            "payload": {
                "headers": [
                    {"name": "From", "value": "Ale <ale@acme.com>"},
                    {"name": "To", "value": "Ed <ed@acme.com>, \"Marciano, Cristiano\" <cm@ey.com>"},
                    {"name": "Cc", "value": "fredrik@engelbrektsloppet.se"},
                    {"name": "Subject", "value": "The quote"},
                    {"name": "Message-ID", "value": "<abc@mail.acme.com>"},
                    {"name": "Date", "value": "Fri, 12 Sep 2025 12:33:00 +0200"}
                ]
            }
        })
    }

    #[test]
    fn a_reply_goes_to_the_address_the_sender_asked_for() {
        assert_eq!(address_of("Ale <ale@acme.com>"), "ale@acme.com");
        assert_eq!(address_of("  ale@acme.com "), "ale@acme.com");
        assert_eq!(address_of("\"Biolchi, Ale\" <ale@acme.com>"), "ale@acme.com");
        assert_eq!(address_of("<>"), "<>", "nothing between them is not an address");

        // Reply-To wins wherever there is one, and the From address otherwise.
        assert_eq!(
            recipient("Ale <ale@acme.com>", Some("Sales <sales@acme.com>")),
            "sales@acme.com"
        );
        assert_eq!(recipient("Ale <ale@acme.com>", None), "ale@acme.com");
        assert_eq!(recipient("Ale <ale@acme.com>", Some("   ")), "ale@acme.com");
    }

    /// A header that names several people is several people: a comma inside a
    /// quoted name is part of the name.
    #[test]
    fn a_header_of_addresses_is_read_one_by_one() {
        assert_eq!(
            addresses("Ale <ale@acme.com>, \"Marciano, Cristiano\" <cm@ey.com>"),
            vec!["ale@acme.com".to_string(), "cm@ey.com".to_string()]
        );
        // A field as it is typed: bare addresses, spaces around them, and a
        // trailing comma that means nothing.
        assert_eq!(
            addresses(" ale@acme.com ,cm@ey.com, "),
            vec!["ale@acme.com".to_string(), "cm@ey.com".to_string()]
        );
        assert_eq!(addresses(""), Vec::<String>::new());
        assert_eq!(addresses("  ,  "), Vec::<String>::new());
        // And back out as one line, which is what the field shows.
        assert_eq!(
            address_line(&addresses("ale@acme.com,cm@ey.com")),
            "ale@acme.com, cm@ey.com"
        );
        assert_eq!(address_line(&[]), "");
    }

    /// A reply answers everybody: the sender in `To`, everyone else in `Cc`,
    /// and never this account itself.
    #[test]
    fn a_reply_answers_everybody_but_the_one_writing_it() {
        let (to, cc) = reply_all(
            "Ale <ale@acme.com>",
            None,
            "Ed <ed@acme.com>, \"Marciano, Cristiano\" <cm@ey.com>",
            "fredrik@norberg.se",
            "ed@acme.com",
        );
        assert_eq!(to, vec!["ale@acme.com".to_string()]);
        assert_eq!(
            cc,
            vec!["cm@ey.com".to_string(), "fredrik@norberg.se".to_string()]
        );

        // The address this account is known by, however it was written down.
        let (to, cc) = reply_all("Ale <ale@acme.com>", None, "ED@ACME.COM", "", " ed@acme.com ");
        assert_eq!(to, vec!["ale@acme.com".to_string()]);
        assert_eq!(cc, Vec::<String>::new());

        // Nobody twice — the sender copied on their own message, and the same
        // person on both lists.
        let (to, cc) = reply_all(
            "Ale <ale@acme.com>",
            None,
            "ale@acme.com, cm@ey.com",
            "CM@ey.com",
            "ed@acme.com",
        );
        assert_eq!(to, vec!["ale@acme.com".to_string()]);
        assert_eq!(cc, vec!["cm@ey.com".to_string()]);

        // Reply-To is still where the reply goes, and the rest are still
        // copied.
        let (to, cc) = reply_all(
            "Ale <ale@acme.com>",
            Some("Sales <sales@acme.com>, orders@acme.com"),
            "ed@acme.com, cm@ey.com",
            "",
            "ed@acme.com",
        );
        assert_eq!(
            to,
            vec!["sales@acme.com".to_string(), "orders@acme.com".to_string()]
        );
        assert_eq!(cc, vec!["cm@ey.com".to_string()]);

        // A message from this account to one person: the copy moves up rather
        // than leaving a reply with nowhere to go.
        let (to, cc) = reply_all("ed@acme.com", None, "ale@acme.com", "", "ed@acme.com");
        assert_eq!(to, vec!["ale@acme.com".to_string()]);
        assert_eq!(cc, Vec::<String>::new());

        // And a message with nobody on it at all is nobody.
        let (to, cc) = reply_all("", None, "", "", "ed@acme.com");
        assert!(to.is_empty() && cc.is_empty());
    }

    #[test]
    fn a_subject_says_re_once_however_many_times_it_has_been_round() {
        assert_eq!(re_subject("The quote"), "Re: The quote");
        assert_eq!(re_subject("Re: The quote"), "Re: The quote");
        assert_eq!(re_subject("RE: The quote"), "RE: The quote");
        assert_eq!(re_subject("re:The quote"), "re:The quote");
        assert_eq!(re_subject("  The quote  "), "Re: The quote");
        assert_eq!(re_subject(""), "Re:");
        // "Reply" starts with the same three letters and is not "Re:".
        assert_eq!(re_subject("Reply by Friday"), "Re: Reply by Friday");
        // A subject shorter than the prefix is not the prefix.
        assert_eq!(re_subject("Re"), "Re: Re");
    }

    #[test]
    fn the_references_chain_grows_by_the_message_being_answered() {
        assert_eq!(references(None, "<abc@acme.com>"), "<abc@acme.com>");
        assert_eq!(
            references(Some("<one@acme.com> <two@acme.com>"), "<abc@acme.com>"),
            "<one@acme.com> <two@acme.com> <abc@acme.com>"
        );
        // Folded over several lines by whoever sent it: one space here.
        assert_eq!(
            references(Some("<one@acme.com>\r\n <two@acme.com>"), "<abc@acme.com>"),
            "<one@acme.com> <two@acme.com> <abc@acme.com>"
        );
        // Already on the end — a chain is not made to repeat itself.
        assert_eq!(
            references(Some("<one@acme.com> <abc@acme.com>"), "<abc@acme.com>"),
            "<one@acme.com> <abc@acme.com>"
        );
        // A message with no id of its own leaves the chain as it was.
        assert_eq!(references(Some("<one@acme.com>"), "  "), "<one@acme.com>");
        assert_eq!(references(None, ""), "");
    }

    #[test]
    fn the_attribution_says_when_and_who() {
        assert_eq!(
            attribution("2025-09-12 12:33", "Ale <ale@acme.com>"),
            "On 2025-09-12 12:33, Ale <ale@acme.com> wrote:"
        );
        assert_eq!(
            attribution("  2025-09-12 12:33 ", " ale@acme.com "),
            "On 2025-09-12 12:33, ale@acme.com wrote:"
        );
        // A message missing one of the two still says what it knows.
        assert_eq!(attribution("", "ale@acme.com"), "ale@acme.com wrote:");
        assert_eq!(
            attribution("2025-09-12 12:33", ""),
            "On 2025-09-12 12:33, the sender wrote:"
        );
        assert_eq!(attribution("", ""), "The message being answered:");

        // Out of the message itself, it is the date the row and the detail
        // header show — Gmail's timestamp where there is one.
        let mut message = original();
        message["internalDate"] = json!("1757673180000");
        assert_eq!(
            attribution_of(&message),
            "On 2025-09-12 10:33, Ale <ale@acme.com> wrote:"
        );
        // No timestamp: the sender's own Date header, as written.
        assert_eq!(
            attribution_of(&original()),
            "On Fri, 12 Sep 2025 12:33:00 +0200, Ale <ale@acme.com> wrote:"
        );
        assert_eq!(attribution_of(&json!({})), "The message being answered:");
    }

    /// What leaves: the words typed, then the message they answer behind
    /// `> `, with its own lines left exactly as they were written.
    #[test]
    fn the_quote_carries_the_original_as_it_was_written() {
        let line = "On 2025-09-12 12:33, Ale <ale@acme.com> wrote:";
        assert_eq!(
            quoted_body("Looks good.", "Can you send the quote?\nBoth days work.", line),
            format!("Looks good.\n\n{line}\n> Can you send the quote?\n> Both days work.")
        );

        // A blank line in the original is a quoted blank line, and nothing is
        // rewrapped however long the line is.
        let long = "a".repeat(200);
        assert_eq!(
            quoted_body("Ja.", &format!("One\n\n{long}"), line),
            format!("Ja.\n\n{line}\n> One\n> \n> {long}")
        );

        // Trailing newlines do not become a quoted empty line on the end.
        assert_eq!(
            quoted_body("Ja.", "One\nTwo\n\n\n", line),
            format!("Ja.\n\n{line}\n> One\n> Two")
        );

        // Nothing to quote is the reply as it was typed.
        assert_eq!(quoted_body("Looks good.", "", line), "Looks good.");
        assert_eq!(quoted_body("Looks good.", "\n\n", line), "Looks good.");

        // Nothing typed yet — the window drafts once before a word — is the
        // quote on its own, with no blank lines above it.
        assert_eq!(
            quoted_body("", "Can you send the quote?", line),
            format!("{line}\n> Can you send the quote?")
        );
        assert_eq!(quoted_body("", "", line), "");
    }

    #[test]
    fn the_draft_is_built_out_of_the_message_being_answered() {
        let draft = draft_of("ed@acme.com", "18f3a2c9b1", "Looks good.", &original(), "");
        assert_eq!(draft.id, "", "Gmail gives it an id when it is written");
        assert_eq!(draft.account, "ed@acme.com");
        assert_eq!(draft.replying_to, "18f3a2c9b1");
        assert_eq!(draft.to, vec!["ale@acme.com".to_string()]);
        assert_eq!(
            draft.cc,
            vec![
                "cm@ey.com".to_string(),
                "fredrik@engelbrektsloppet.se".to_string()
            ],
            "everyone else on it, and never this account"
        );
        assert_eq!(draft.subject, "Re: The quote");
        assert_eq!(draft.body, "Looks good.");
        assert_eq!(draft.in_reply_to, "<abc@mail.acme.com>");
        // And with the original's text to hand, the body is the whole of what
        // would leave: the words typed, then the message they answer.
        let quoting = draft_of(
            "ed@acme.com",
            "18f3a2c9b1",
            "Looks good.",
            &original(),
            "Can you send the quote?",
        );
        assert_eq!(
            quoting.body,
            "Looks good.\n\nOn Fri, 12 Sep 2025 12:33:00 +0200, Ale <ale@acme.com> wrote:\n\
             > Can you send the quote?"
        );
        assert_eq!(draft.references, "<abc@mail.acme.com>");
        assert_eq!(draft.thread_id, "18f3a2c9b0", "so Gmail threads it");

        // A message with nothing in it is not a panic — it is a draft with
        // nobody to send it to, which `draft` refuses.
        let bare = draft_of("ed@acme.com", "18f3", "hi", &json!({}), "");
        assert!(bare.to.is_empty() && bare.cc.is_empty());
        assert_eq!(bare.subject, "Re:");
        assert_eq!(bare.thread_id, "");
    }

    /// A draft as Gmail holds it comes back as the draft it was: the
    /// addresses, Bcc included, the subject, the words, the thread and the
    /// chain that keep a reply a reply, and its files by name.
    #[test]
    fn a_gmail_draft_reads_back_whole() {
        let b64 = |text: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text);
        let message = json!({
            "id": "18f9",
            "threadId": "18f3a",
            "payload": {
                "mimeType": "multipart/mixed",
                "headers": [
                    {"name": "To", "value": "Ale <ale@acme.com>"},
                    {"name": "Cc", "value": "cm@ey.com"},
                    {"name": "Bcc", "value": "fredrik@norberg.se"},
                    {"name": "Subject", "value": "Re: Offerten"},
                    {"name": "In-Reply-To", "value": "<abc@acme.com>"},
                    {"name": "References", "value": "<one@acme.com> <abc@acme.com>"}
                ],
                "parts": [
                    {"mimeType": "text/plain", "body": {"data": b64("Tack — det ser bra ut.")}},
                    {"mimeType": "application/pdf", "filename": "offert.pdf",
                     "body": {"attachmentId": "att-1", "size": 1024}}
                ]
            }
        });
        let draft = draft_of_gmail("ed@acme.com", "r-123", &message);
        assert_eq!(draft.id, "r-123");
        assert_eq!(draft.to, vec!["ale@acme.com".to_string()]);
        assert_eq!(draft.cc, vec!["cm@ey.com".to_string()]);
        assert_eq!(draft.bcc, vec!["fredrik@norberg.se".to_string()]);
        assert_eq!(draft.subject, "Re: Offerten");
        assert_eq!(draft.body, "Tack — det ser bra ut.");
        assert_eq!(draft.in_reply_to, "<abc@acme.com>");
        assert_eq!(draft.thread_id, "18f3a");
        assert_eq!(draft.attachments, vec!["offert.pdf".to_string()]);
    }

    /// A forward says `Fwd:` once, and carries the lines every client writes
    /// above a forwarded message.
    #[test]
    fn a_forward_says_what_it_forwards() {
        assert_eq!(fwd_subject("Offerten"), "Fwd: Offerten");
        assert_eq!(fwd_subject("Fwd: Offerten"), "Fwd: Offerten");
        assert_eq!(fwd_subject("FW: Offerten"), "FW: Offerten");
        assert_eq!(fwd_subject(""), "Fwd:");

        let header = forwarded_header(&original());
        assert!(header.starts_with("---------- Forwarded message ---------"), "{header}");
        assert!(header.contains("From: "), "{header}");
        assert!(header.contains("Subject: "), "{header}");
    }

    /// A draft may go to nobody yet — it is being written — but an address
    /// that is not one is refused wherever it is.
    #[test]
    fn a_draft_may_go_to_nobody_yet() {
        let mut draft = Draft::default();
        assert_eq!(misaddressed(&draft), None);
        draft.bcc = vec!["fredrik".to_string()];
        assert!(misaddressed(&draft).is_some_and(|e| e.contains("fredrik")));
        assert!(empty(&Draft::default(), "  "));
        assert!(!empty(&Draft::default(), "hej"));
    }

    /// What actually leaves: the headers that thread it, and a body no mail
    /// server has to think about.
    #[test]
    fn the_raw_message_carries_the_headers_that_thread_it() {
        let mut draft = draft_of("ed@acme.com", "18f3a2c9b1", "Looks good.", &original(), "");
        let raw = raw_message(&draft).expect("a message");
        assert!(raw.starts_with("To: ale@acme.com\r\n"), "got {raw}");
        assert!(
            raw.contains("\r\nCc: cm@ey.com, fredrik@engelbrektsloppet.se\r\n"),
            "got {raw}"
        );
        assert!(raw.contains("\r\nSubject: Re: The quote\r\n"));
        assert!(raw.contains("\r\nIn-Reply-To: <abc@mail.acme.com>\r\n"));
        assert!(raw.contains("\r\nReferences: <abc@mail.acme.com>\r\n"));
        assert!(raw.contains("\r\nMIME-Version: 1.0\r\n"));
        assert!(raw.contains("Content-Transfer-Encoding: base64\r\n"));

        // The body is the base64 after the blank line, and it decodes back.
        let body = raw.split("\r\n\r\n").nth(1).expect("a body").trim();
        let bytes = crate::mime::b64url(&body.replace("\r\n", "")).expect("base64");
        assert_eq!(String::from_utf8(bytes).expect("utf-8"), "Looks good.");

        // A subject that is not 7-bit travels encoded, and a long body is
        // folded into lines a mail server will carry.
        draft.subject = "Re: Offerten är klar".to_string();
        draft.body = "å".repeat(400);
        let raw = raw_message(&draft).expect("a message");
        assert!(raw.contains("Subject: =?UTF-8?B?"), "got {raw}");
        let body = raw.split("\r\n\r\n").nth(1).expect("a body");
        assert!(
            body.lines().all(|line| line.trim_end().len() <= 76),
            "a line came out too long"
        );
        let bytes = crate::mime::b64url(&body.replace("\r\n", "")).expect("base64");
        assert_eq!(String::from_utf8(bytes).expect("utf-8"), "å".repeat(400));

        // A message with no id of its own carries neither header rather than
        // an empty one.
        let bare = draft_of("ed@acme.com", "18f3", "hi", &json!({}), "");
        let raw = raw_message(&bare).expect("a message");
        assert!(!raw.contains("In-Reply-To"), "got {raw}");
        assert!(!raw.contains("References"), "got {raw}");
        assert!(!raw.contains("Cc:"), "nobody copied is no header: {raw}");
    }

    #[test]
    fn a_header_is_itself_until_it_is_not_ascii() {
        assert_eq!(header_value("Re: The quote"), "Re: The quote");
        assert_eq!(header_value(""), "");
        assert_eq!(header_value("Re: Offerten"), "Re: Offerten");
        let encoded = header_value("Re: Är den klar?");
        assert!(encoded.starts_with("=?UTF-8?B?") && encoded.ends_with("?="));
        let inner = encoded
            .trim_start_matches("=?UTF-8?B?")
            .trim_end_matches("?=");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(inner)
            .expect("base64");
        assert_eq!(String::from_utf8(bytes).expect("utf-8"), "Re: Är den klar?");
    }
}
