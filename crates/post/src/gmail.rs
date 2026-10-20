//! The Gmail REST calls Post needs, and nothing else.
//!
//! Plain HTTPS with `ureq`: no async runtime, no SDK, one thread per call and
//! the call is over when the function returns.
//!
//! A listing is one round trip for the ids and one batch call for the headers,
//! and the newest page of each mailbox is kept on disk so a window has mail to
//! draw before the network has answered. Gmail stays authoritative: the cached
//! page is handed over and a refresh is already on its way behind it.
//!
//! OAuth tokens are refreshed here, quietly: an access token near its expiry,
//! or a 401 from an access token revoked early, is exchanged for a new one and
//! the call is retried once. The account only ever has to be connected once.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use crate::store::OauthTokens;
use crate::{mime, oauth, paths, store};

/// Everything is addressed as `me`: the access token says which mailbox that
/// is.
const API: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

/// Where many small GETs go as one call.
const BATCH_API: &str = "https://gmail.googleapis.com/batch/gmail/v1";

/// How many inner calls go in one batch. Gmail allows a hundred; a page is
/// smaller than that, so this is only ever one call.
const BATCH: usize = 50;

/// The separator between the inner calls we send. Ours to choose, and it may
/// not appear in anything it separates — nothing here contains it.
const BOUNDARY: &str = "armor-post-batch";

/// One page of a mailbox. Small on purpose: a page is a thing a model reads,
/// not a mailbox dump.
pub const PAGE: usize = 25;

/// What shape a saved page is in. A page saved before a row grew a field, or
/// before the listing became the inbox, is the wrong page: it is thrown away
/// and fetched again rather than drawn. Bump this whenever a row changes.
const CACHE: u64 = 3;

/// The shared HTTP client. Timeouts, because a hung call is worse than a failed
/// one.
pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .user_agent(concat!("armor-post/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// Which mailbox an access token belongs to. Used once, while connecting an
/// account: it is how Post learns the address to file the OAuth tokens under.
pub fn profile(access: &str) -> Result<String, String> {
    let answer = send(access, "GET", &format!("{API}/profile"), None).map_err(|e| e.message())?;
    answer
        .get("emailAddress")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "Gmail did not say which address this is".to_string())
}

/// One page of a mailbox, newest first: who it is from, what it is about, when,
/// and whether it has been read.
///
/// The newest page comes back from the cache the instant there is one, with
/// `"cached": true` on it and a refresh already running behind it — but only
/// where somebody is [`subscribe`]d to hear that refresh land. Nowhere else is
/// there anything to hand a fresher page to, so a shim or a terminal fetches
/// for itself and answers with what Gmail just said.
pub fn list(email: &str, page: Option<&str>) -> Result<Value, String> {
    let page = page.filter(|t| !t.trim().is_empty());
    // A later page is not the newest one, so it is never the cached one.
    if page.is_some() {
        return Ok(flagged(fetch(email, page)?, false));
    }
    if watched()
        && let Some(cached) = cached_page(email)
    {
        refresh_in_background(email);
        return Ok(flagged(cached, true));
    }
    let answer = fetch(email, None)?;
    save_page(email, &answer);
    Ok(flagged(answer, false))
}

/// One page of a Gmail search across all of an account's mail, sent included —
/// how someone's address is found. Never cached: a search is asked once.
pub fn search(email: &str, query: &str, page: Option<&str>) -> Result<Value, String> {
    let page = page.filter(|t| !t.trim().is_empty());
    Ok(flagged(fetch_matching(email, None, Some(query), page)?, false))
}

/// One page of one folder — a label, by its id — newest first, and with a
/// query only what matches it there. Fetched every time: only the inbox is
/// kept on disk.
pub fn folder(
    email: &str,
    label: &str,
    query: Option<&str>,
    page: Option<&str>,
) -> Result<Value, String> {
    let page = page.filter(|t| !t.trim().is_empty());
    let query = query.filter(|q| !q.trim().is_empty());
    Ok(flagged(fetch_matching(email, Some(label), query, page)?, false))
}

/// One page, straight from Gmail: the ids, then their headers in one batch.
///
/// The inbox, not all mail: a listing is what arrived, so a reply of this
/// account's own is not a line of its own. A conversation still shows every
/// message on it — `threads.get` is not filtered.
fn fetch(email: &str, page: Option<&str>) -> Result<Value, String> {
    fetch_matching(email, Some("INBOX"), None, page)
}

/// One page of a label, of a search, or of a search within a label.
fn fetch_matching(
    email: &str,
    label: Option<&str>,
    query: Option<&str>,
    page: Option<&str>,
) -> Result<Value, String> {
    // The Drafts folder is listed as drafts, so every row carries the id it
    // is opened, sent and discarded by.
    if label == Some("DRAFT") && query.is_none() {
        return fetch_drafts(email, page);
    }
    let mut url = format!("{API}/messages?maxResults={PAGE}");
    if let Some(label) = label {
        url.push_str(&format!("&labelIds={}", oauth::urlencode(checked_label(label)?)));
        // Spam and trash are left out of every listing unless asked for.
        if matches!(label, "SPAM" | "TRASH") {
            url.push_str("&includeSpamTrash=true");
        }
    }
    if let Some(query) = query {
        url.push_str(&format!("&q={}", oauth::urlencode(query)));
    }
    if let Some(token) = page {
        url.push_str(&format!("&pageToken={}", oauth::urlencode(token)));
    }
    let index = request(email, "GET", &url, None)?;

    let ids: Vec<String> = index
        .get("messages")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    Ok(json!({
        "account": email,
        "label": label,
        "messages": headers(email, &ids)?,
        "next_page": index.get("nextPageToken").cloned().unwrap_or(Value::Null),
    }))
}

/// One page of the account's drafts: rows like any listing's, each with its
/// `draft_id`.
fn fetch_drafts(email: &str, page: Option<&str>) -> Result<Value, String> {
    let mut url = format!("{API}/drafts?maxResults={PAGE}");
    if let Some(token) = page {
        url.push_str(&format!("&pageToken={}", oauth::urlencode(token)));
    }
    let index = request(email, "GET", &url, None)?;
    let drafts: Vec<(String, String)> = index
        .get("drafts")
        .and_then(Value::as_array)
        .map(|drafts| {
            drafts
                .iter()
                .filter_map(|draft| {
                    Some((
                        draft.get("id")?.as_str()?.to_string(),
                        draft.get("message")?.get("id")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let ids: Vec<String> = drafts.iter().map(|(_, message)| message.clone()).collect();
    let mut rows = headers(email, &ids)?;
    for row in &mut rows {
        let id = row.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        if let Some((draft, _)) = drafts.iter().find(|(_, message)| *message == id) {
            row["draft_id"] = json!(draft);
        }
    }
    Ok(json!({
        "account": email,
        "label": "DRAFT",
        "messages": rows,
        "next_page": index.get("nextPageToken").cloned().unwrap_or(Value::Null),
    }))
}

/// The system folders that hold mail, in the order Gmail shows them, with the
/// names it shows them under.
const SYSTEM: [(&str, &str); 7] = [
    ("INBOX", "Inbox"),
    ("STARRED", "Starred"),
    ("IMPORTANT", "Important"),
    ("SENT", "Sent"),
    ("DRAFT", "Drafts"),
    ("SPAM", "Spam"),
    ("TRASH", "Trash"),
];

/// The account's folders as Gmail has them: the system ones that hold mail,
/// then its own labels by name — a nested one is `Parent/Child`, right after
/// its parent — each with how many of its conversations are unread and how
/// many messages it holds. Category
/// and chat labels are not folders, and a label hidden from Gmail's list is
/// not in this one either.
pub fn labels(email: &str) -> Result<Value, String> {
    let index = request(email, "GET", &format!("{API}/labels"), None)?;
    let listed = index
        .get("labels")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut ids: Vec<String> = SYSTEM
        .iter()
        .filter(|(id, _)| listed.iter().any(|l| l.get("id").and_then(Value::as_str) == Some(id)))
        .map(|(id, _)| id.to_string())
        .collect();
    let mut own: Vec<(String, String)> = listed
        .iter()
        .filter(|l| l.get("type").and_then(Value::as_str) == Some("user"))
        .filter(|l| l.get("labelListVisibility").and_then(Value::as_str) != Some("labelHide"))
        .filter_map(|l| {
            Some((
                l.get("name")?.as_str()?.to_string(),
                l.get("id")?.as_str()?.to_string(),
            ))
        })
        .collect();
    own.sort_by_key(|(name, _)| name.to_lowercase());
    ids.extend(own.into_iter().map(|(_, id)| id));

    // The counts are on each label's own answer: one batch for all of them.
    let paths: Vec<String> = ids
        .iter()
        .map(|id| Ok(format!("/gmail/v1/users/me/labels/{}", checked_id(id)?)))
        .collect::<Result<_, String>>()?;
    let mut counted: HashMap<String, Value> = HashMap::new();
    for chunk in paths.chunks(BATCH) {
        for label in batch_get(email, chunk)? {
            if let Some(id) = label.get("id").and_then(Value::as_str) {
                counted.insert(id.to_string(), label);
            }
        }
    }
    let folders: Vec<Value> = ids
        .iter()
        .filter_map(|id| counted.remove(id))
        .map(|label| {
            let id = label.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = SYSTEM
                .iter()
                .find(|(system, _)| *system == id)
                .map(|(_, name)| name.to_string())
                .unwrap_or_else(|| {
                    label.get("name").and_then(Value::as_str).unwrap_or(id).to_string()
                });
            json!({
                "id": id,
                "name": name,
                "system": label.get("type").and_then(Value::as_str) == Some("system"),
                "unread": label.get("threadsUnread").and_then(Value::as_u64).unwrap_or(0),
                // What Drafts counts: how many there are, none of them unread.
                "total": label.get("messagesTotal").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .collect();
    Ok(json!({"account": email, "labels": folders}))
}

/// The listing rows for a page of ids. The list endpoint hands out ids only,
/// so the headers are a second call — one call, not one per message.
fn headers(email: &str, ids: &[String]) -> Result<Vec<Value>, String> {
    let mut rows = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(BATCH) {
        rows.extend(batch(email, chunk)?);
    }
    Ok(rows)
}

/// One batch call: N metadata GETs in a multipart body, N answers in a
/// multipart answer. Metadata format — no bodies over the wire for a listing.
fn batch(email: &str, ids: &[String]) -> Result<Vec<Value>, String> {
    let paths: Vec<String> = ids
        .iter()
        .map(|id| {
            Ok(format!(
                "/gmail/v1/users/me/messages/{}?format=metadata\
                 &metadataHeaders=From&metadataHeaders=To\
                 &metadataHeaders=Subject&metadataHeaders=Date",
                checked_id(id)?
            ))
        })
        .collect::<Result<_, String>>()?;
    // Gmail answers in whatever order it likes, so the page's own order wins.
    // Anything that did not come back whole is simply not in the page.
    let mut by_id: HashMap<String, Value> = batch_get(email, &paths)?
        .into_iter()
        .filter_map(|message| {
            let id = message.get("id").and_then(Value::as_str)?.to_string();
            Some((id, message))
        })
        .collect();
    Ok(ids
        .iter()
        .filter_map(|id| by_id.remove(id.trim()).as_ref().map(summary))
        .collect())
}

/// GETs as one batch call, the answers in whatever order Gmail gives them.
fn batch_get(email: &str, paths: &[String]) -> Result<Vec<Value>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut body = String::new();
    for (n, path) in paths.iter().enumerate() {
        body.push_str(&format!(
            "--{BOUNDARY}\r\n\
             Content-Type: application/http\r\n\
             Content-ID: <{n}>\r\n\r\n\
             GET {path}\r\n\r\n"
        ));
    }
    body.push_str(&format!("--{BOUNDARY}--\r\n"));

    let access = access_token(email, false)?;
    match send_batch(&access, &body) {
        Err(Failure::Unauthorized) => {
            let access = access_token(email, true)?;
            send_batch(&access, &body).map_err(|e| e.message())
        }
        other => other.map_err(|e| e.message()),
    }
}

/// One message's row in a listing, out of Gmail's JSON.
pub fn summary(message: &Value) -> Value {
    let payload = message.get("payload").unwrap_or(&Value::Null);
    let labelled = |label: &str| {
        message
            .get("labelIds")
            .and_then(Value::as_array)
            .is_some_and(|labels| labels.iter().any(|l| l.as_str() == Some(label)))
    };
    json!({
        "id": message.get("id").and_then(Value::as_str).unwrap_or_default(),
        // Which conversation this message is on. Gmail puts it on every shape
        // a message comes back in, so a listing row carries it and the rows
        // group without a second call.
        "thread_id": message.get("threadId").and_then(Value::as_str).unwrap_or_default(),
        "from": mime::header(payload, "From").unwrap_or_default(),
        "to": mime::header(payload, "To").unwrap_or_default(),
        "subject": mime::header(payload, "Subject").unwrap_or_default(),
        "date": when(message, payload),
        "unread": labelled("UNREAD"),
        // Whether this account wrote it. An alias sends under another address,
        // so the label is what says it, not the From line.
        "sent": labelled("SENT"),
        // The start of the message, as Gmail cut it — HTML-escaped there.
        "snippet": mime::entities(message.get("snippet").and_then(Value::as_str).unwrap_or_default()),
    })
}

/// When a message arrived, as `YYYY-MM-DD HH:MM` UTC. Gmail's own timestamp
/// when it gave one; the sender's `Date:` header otherwise, as written.
fn when(message: &Value, payload: &Value) -> String {
    let internal = message
        .get("internalDate")
        .and_then(|d| match d {
            Value::String(s) => s.parse::<i64>().ok(),
            other => other.as_i64(),
        })
        .map(mime::stamp);
    internal
        .or_else(|| mime::header(payload, "Date"))
        .unwrap_or_default()
}

/// One message's body as text, and the message marked read.
///
/// The text part is preferred; an HTML-only message is stripped to text. The
/// HTML itself, when there is any, is written to the state dir so a window — or
/// a person — can look at the real thing.
///
/// A message that has been read once is read from disk every time after: a
/// message does not change, so there is nothing to refresh and nothing to mark
/// — it was marked read the first time. Opening it again costs no network.
pub fn read(email: &str, id: &str) -> Result<Value, String> {
    let id = checked_id(id)?;
    if let Some(cached) = cached_message(email, id) {
        return Ok(cached);
    }
    let url = format!("{API}/messages/{id}?format=full");
    let message = request(email, "GET", &url, None)?;
    // The plan's rule: reading a message reads it. Failing to set the flag does
    // not lose the body we already have.
    let marked = mark(email, id, true).is_ok();
    let answer = whole(email, &message, marked);
    save_message(email, id, &answer);
    Ok(answer)
}

/// One message in full, out of Gmail's JSON: its row, its body as text, the
/// HTML written to the state dir, and what is attached to it. `marked` is
/// whether its unread flag has been cleared — a read message says so, and a
/// message whose mark did not go through is not kept.
///
/// Shared by the one message [`read`] opens and every message in a
/// conversation, so a member of a thread is exactly what `post_read` would
/// have answered with.
fn whole(email: &str, message: &Value, marked: bool) -> Value {
    let id = message.get("id").and_then(Value::as_str).unwrap_or_default();
    let payload = message.get("payload").cloned().unwrap_or_else(|| json!({}));
    let body = mime::body(&payload);

    let text = match (&body.text, &body.html) {
        (Some(text), _) => text.clone(),
        (None, Some(html)) => mime::strip_html(html),
        (None, None) => String::new(),
    };

    let html_path = match &body.html {
        Some(html) => {
            let file = paths::body_file(email, id);
            match store::create(&file) {
                Ok(mut out) => {
                    use std::io::Write;
                    match out.write_all(html.as_bytes()) {
                        Ok(()) => json!(file.display().to_string()),
                        Err(_) => Value::Null,
                    }
                }
                // A body that could not be saved is not a failed read.
                Err(_) => Value::Null,
            }
        }
        None => Value::Null,
    };

    let attachments: Vec<Value> = mime::attachments(&payload)
        .iter()
        .map(|a| json!({"filename": a.filename, "mime": a.mime, "bytes": a.size}))
        .collect();

    let mut answer = summary(message);
    let row = answer.as_object_mut().expect("an object");
    row.insert("text".to_string(), json!(mime::cap(&text, mime::MAX_TEXT)));
    row.insert("html_path".to_string(), html_path);
    row.insert("attachments".to_string(), json!(attachments));
    row.insert("unread".to_string(), json!(!marked));
    row.insert("marked_read".to_string(), json!(marked));
    answer
}

/// One conversation: every message on it, oldest first, each as [`read`] would
/// answer for it — and the unread ones among them read, because showing a
/// message is reading it.
///
/// One call for the whole thread rather than one per message. A member already
/// read once comes off the disk, exactly as a second `post_read` of it would.
/// Each message's text has the quote under it taken off: the message it quotes
/// is the one above it in the same answer.
pub fn thread(email: &str, thread_id: &str) -> Result<Value, String> {
    let fetched = fetch_thread(email, thread_id)?;
    let id = fetched
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or(thread_id.trim())
        .to_string();
    let members: Vec<Value> = fetched
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if members.is_empty() {
        return Err(format!("there is no conversation {id} in {email}"));
    }

    // One modify for the whole conversation: the unread members are exactly
    // the ones being shown, and reading them is what this call does.
    let any_unread = members
        .iter()
        .any(|m| summary(m)["unread"] == Value::Bool(true));
    let marked = !any_unread || mark_thread(email, &id).is_ok();

    let mut messages = Vec::with_capacity(members.len());
    for member in &members {
        let mid = member.get("id").and_then(Value::as_str).unwrap_or_default();
        let answer = match cached_message(email, mid) {
            Some(cached) => cached,
            None => {
                let answer = whole(email, member, marked);
                save_message(email, mid, &answer);
                answer
            }
        };
        if marked {
            cache_unread(email, mid, false);
        }
        let mut compact = answer;
        if let Some(row) = compact.as_object_mut() {
            let said = row
                .get("text")
                .and_then(Value::as_str)
                .map(mime::unquote)
                .unwrap_or_default();
            row.insert("text".to_string(), json!(said));
        }
        messages.push(compact);
    }

    // What the conversation is about is what its newest message calls it: a
    // subject that changed mid-thread changed for everybody.
    let subject = messages
        .iter()
        .rev()
        .find_map(|m| {
            m.get("subject")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_default()
        .to_string();

    Ok(json!({
        "account": email,
        "thread_id": id,
        "subject": subject,
        "count": messages.len(),
        "messages": messages,
    }))
}

/// The thread Gmail has under this id — or, when the id turns out to be a
/// message's rather than a conversation's, the conversation that message is
/// on. `post read <email> <id>` carries a message id and nothing else,
/// and it opens the conversation like every other road in.
fn fetch_thread(email: &str, thread_id: &str) -> Result<Value, String> {
    let id = checked_id(thread_id)?;
    let conversation =
        |id: &str| request(email, "GET", &format!("{API}/threads/{id}?format=full"), None);
    match conversation(id) {
        Err(refused) if refused.contains("(404)") => {
            let minimal = request(
                email,
                "GET",
                &format!("{API}/messages/{id}?format=minimal"),
                None,
            )
            .map_err(|_| refused.clone())?;
            let on = minimal
                .get("threadId")
                .and_then(Value::as_str)
                .ok_or(refused)?
                .to_string();
            conversation(checked_id(&on)?)
        }
        other => other,
    }
}

/// Clears the unread flag on every message of one conversation, in one call.
fn mark_thread(email: &str, thread_id: &str) -> Result<Value, String> {
    let url = format!("{API}/threads/{}/modify", checked_id(thread_id)?);
    request(email, "POST", &url, Some(json!({"removeLabelIds": ["UNREAD"]})))
}

/// One message's headers, for building a reply to it: who it is from, where
/// they asked to be answered, who else was on it — a reply answers everybody —
/// what it is about, when it was written — the attribution line above the quote
/// needs that — and the two ids that thread a conversation. Metadata format, so
/// no body comes over the wire for it.
pub fn original(email: &str, id: &str) -> Result<Value, String> {
    let url = format!(
        "{API}/messages/{}?format=metadata\
         &metadataHeaders=From&metadataHeaders=Reply-To&metadataHeaders=Subject\
         &metadataHeaders=To&metadataHeaders=Cc\
         &metadataHeaders=Message-ID&metadataHeaders=References&metadataHeaders=Date",
        checked_id(id)?
    );
    request(email, "GET", &url, None)
}

// ------------------------------------------------------------------- drafts

/// Writes a draft into the account's Drafts — a new one, or over `draft_id` —
/// where Gmail on every other device sees it. `raw` is the whole message,
/// base64url; `thread_id` keeps a reply on its conversation.
pub fn draft_save(
    email: &str,
    draft_id: Option<&str>,
    raw: &str,
    thread_id: &str,
) -> Result<Value, String> {
    let mut message = json!({ "raw": raw });
    let thread_id = thread_id.trim();
    if !thread_id.is_empty() {
        message["threadId"] = json!(checked_id(thread_id)?);
    }
    match draft_id {
        Some(id) => {
            let id = checked_draft(id)?;
            request(
                email,
                "PUT",
                &format!("{API}/drafts/{id}"),
                Some(json!({ "id": id, "message": message })),
            )
        }
        None => request(email, "POST", &format!("{API}/drafts"), Some(json!({ "message": message }))),
    }
}

/// One draft, its message whole.
pub fn draft_get(email: &str, draft_id: &str) -> Result<Value, String> {
    let url = format!("{API}/drafts/{}?format=full", checked_draft(draft_id)?);
    request(email, "GET", &url, None)
}

/// Sends a draft as Gmail holds it. The one call in this file that puts mail
/// on the wire.
pub fn draft_send(email: &str, draft_id: &str) -> Result<Value, String> {
    let body = json!({ "id": checked_draft(draft_id)? });
    request(email, "POST", &format!("{API}/drafts/send"), Some(body))
}

/// Deletes a draft for good: Gmail keeps no trash for drafts.
pub fn draft_delete(email: &str, draft_id: &str) -> Result<Value, String> {
    let url = format!("{API}/drafts/{}", checked_draft(draft_id)?);
    request(email, "DELETE", &url, None)
}

/// Sets or clears the unread flag.
pub fn mark(email: &str, id: &str, read: bool) -> Result<Value, String> {
    let url = format!("{API}/messages/{}/modify", checked_id(id)?);
    let body = if read {
        json!({"removeLabelIds": ["UNREAD"]})
    } else {
        json!({"addLabelIds": ["UNREAD"]})
    };
    request(email, "POST", &url, Some(body))?;
    // The cached page would otherwise still call it unread until the next
    // refresh, and a row that lies is worse than a row that is a second old.
    cache_unread(email, id.trim(), !read);
    // A message put back to unread is a message waiting to be read again, so
    // its cached read goes: the next open fetches it and marks it read.
    if !read {
        forget_message(email, id.trim());
    }
    Ok(json!({"id": id, "account": email, "read": read}))
}

/// Moves a message to the trash, where Gmail keeps it for 30 days. Nothing here
/// deletes anything for good.
pub fn trash(email: &str, id: &str) -> Result<Value, String> {
    let url = format!("{API}/messages/{}/trash", checked_id(id)?);
    request(email, "POST", &url, Some(json!({})))?;
    cache_drop(email, id.trim());
    forget_message(email, id.trim());
    Ok(json!({"id": id, "account": email, "trashed": true}))
}

/// Saves one attachment into the download folder, without overwriting
/// anything already there.
pub fn attachment(email: &str, id: &str, name: Option<&str>) -> Result<Value, String> {
    let url = format!("{API}/messages/{}?format=full", checked_id(id)?);
    let message = request(email, "GET", &url, None)?;
    let payload = message.get("payload").cloned().unwrap_or_else(|| json!({}));
    let files = mime::attachments(&payload);
    if files.is_empty() {
        return Err(format!("message {id} has no attachments"));
    }
    let names: Vec<&str> = files.iter().map(|a| a.filename.as_str()).collect();
    let wanted = match name.filter(|n| !n.trim().is_empty()) {
        Some(name) => files
            .iter()
            .find(|a| a.filename.eq_ignore_ascii_case(name.trim()))
            .ok_or_else(|| {
                format!(
                    "message {id} has no attachment called \"{name}\" — it has: {}",
                    names.join(", ")
                )
            })?,
        None if files.len() == 1 => &files[0],
        // Never guess which file was meant.
        None => {
            return Err(format!(
                "message {id} has {} attachments — pass one of these names: {}",
                files.len(),
                names.join(", ")
            ));
        }
    };

    let url = format!(
        "{API}/messages/{}/attachments/{}",
        checked_id(id)?,
        checked_id(&wanted.id)?
    );
    let fetched = request(email, "GET", &url, None)?;
    let data = fetched
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Gmail sent no bytes for {}", wanted.filename))?;
    let bytes = mime::b64url(data)?;

    let dir = paths::attachments_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let path = mime::unique_path(&dir, &wanted.filename);
    std::fs::write(&path, &bytes).map_err(|e| format!("could not write {}: {e}", path.display()))?;

    Ok(json!({
        "path": path.display().to_string(),
        "filename": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        "mime": wanted.mime,
        "bytes": bytes.len(),
    }))
}

/// An id out of a model or a shell is text: it goes into a URL path, so it has
/// to look like an id.
fn checked_id(id: &str) -> Result<&str, String> {
    let id = id.trim();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!(
            "\"{id}\" is not a message id — pass back the id from post_list, verbatim"
        ));
    }
    Ok(id)
}

/// A draft id as Gmail writes them — `r-123…` — and nothing that could be
/// more than one word of a URL.
fn checked_draft(id: &str) -> Result<&str, String> {
    let id = id.trim();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!(
            "\"{id}\" is not a draft id — pass back the draft_id post_draft or post_new \
             answered with, verbatim"
        ));
    }
    Ok(id)
}

/// A label id as Gmail writes them — `INBOX`, `Label_12` — and nothing that
/// could be more than one word of a URL.
fn checked_label(label: &str) -> Result<&str, String> {
    let label = label.trim();
    if label.is_empty() || !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!(
            "\"{label}\" is not a label id — pass an id from post_labels, e.g. SENT or Label_12"
        ));
    }
    Ok(label)
}

/// One call, with the access token refreshed if it has to be.
fn request(email: &str, method: &str, url: &str, body: Option<Value>) -> Result<Value, String> {
    let access = access_token(email, false)?;
    match send(&access, method, url, body.clone()) {
        Err(Failure::Unauthorized) => {
            // The access token was refused: get a new one and try once more. A
            // second refusal is the account's problem, not the token's.
            let access = access_token(email, true)?;
            send(&access, method, url, body).map_err(|e| e.message())
        }
        other => other.map_err(|e| e.message()),
    }
}

/// A usable access token for one account, refreshing when it is due — or when
/// `force`, which is what a 401 means.
/// The one sentence a dead grant produces, and the one test for it — the
/// window offers its Reconnect button on this, so it is defined in one place.
pub fn reconnect_error(email: &str) -> String {
    format!("the Google sign-in for {email} has expired — reconnect the account")
}

pub fn reconnect_needed(error: &str) -> bool {
    error.starts_with("the Google sign-in for") && error.contains("expired")
}

fn access_token(email: &str, force: bool) -> Result<String, String> {
    let dir = paths::oauth_tokens_dir();
    let tokens = store::load_oauth_tokens(&dir, email)?;
    // A minute of slack: an access token about to expire is not worth sending.
    if !force && !tokens.access_token.is_empty() && tokens.expiry > store::now() + 60 {
        return Ok(tokens.access_token);
    }
    if tokens.refresh_token.is_empty() {
        return Err(reconnect_error(email));
    }
    let client = store::client()?;
    let fresh = oauth::refresh(&client, &tokens.refresh_token).map_err(|e| match e {
        oauth::RefreshError::Reauth => reconnect_error(email),
        oauth::RefreshError::Other(text) => text,
    })?;
    // Google does not resend the refresh token, so the stored one stays.
    let merged = OauthTokens {
        refresh_token: if fresh.refresh_token.is_empty() {
            tokens.refresh_token
        } else {
            fresh.refresh_token
        },
        ..fresh
    };
    store::save_oauth_tokens(&dir, email, &merged)?;
    Ok(merged.access_token)
}

/// What a call can go wrong as. A 401 is its own case because it is the one
/// worth retrying.
enum Failure {
    Unauthorized,
    Message(String),
}

impl Failure {
    fn message(self) -> String {
        match self {
            Failure::Unauthorized => {
                "Gmail refused the access token, twice — reconnect with: post add-account"
                    .to_string()
            }
            Failure::Message(text) => text,
        }
    }
}

/// One HTTP call to Gmail, with the access token already decided.
fn send(access: &str, method: &str, url: &str, body: Option<Value>) -> Result<Value, Failure> {
    let agent = agent();
    let request = match method {
        "POST" => agent.post(url),
        "PUT" => agent.put(url),
        "DELETE" => agent.delete(url),
        _ => agent.get(url),
    }
    .set("Authorization", &format!("Bearer {access}"))
    .set("Accept", "application/json");

    let outcome = match body {
        Some(body) => request
            .set("Content-Type", "application/json")
            .send_string(&body.to_string()),
        None => request.call(),
    };

    match outcome {
        Ok(response) => {
            let text = response
                .into_string()
                .map_err(|e| Failure::Message(format!("Gmail's answer stopped short: {e}")))?;
            if text.trim().is_empty() {
                return Ok(json!({}));
            }
            serde_json::from_str(&text)
                .map_err(|e| Failure::Message(format!("Gmail sent something unreadable: {e}")))
        }
        Err(ureq::Error::Status(401, _)) => Err(Failure::Unauthorized),
        Err(ureq::Error::Status(code, response)) => {
            let text = response.into_string().unwrap_or_default();
            Err(Failure::Message(explain(code, &text)))
        }
        Err(e) => Err(Failure::Message(format!("could not reach Gmail: {e}"))),
    }
}

/// One batch call, with the access token already decided: the multipart body
/// goes up, and the JSON out of each part comes back.
fn send_batch(access: &str, body: &str) -> Result<Vec<Value>, Failure> {
    let outcome = agent()
        .post(BATCH_API)
        .set("Authorization", &format!("Bearer {access}"))
        .set("Content-Type", &format!("multipart/mixed; boundary={BOUNDARY}"))
        .send_string(body);

    match outcome {
        Ok(response) => {
            // The boundary of the answer is Gmail's, not ours, and the header
            // has to be read before the body is taken.
            let kind = response.header("Content-Type").unwrap_or_default().to_string();
            let text = response
                .into_string()
                .map_err(|e| Failure::Message(format!("Gmail's answer stopped short: {e}")))?;
            unbatch(&kind, &text)
        }
        Err(ureq::Error::Status(401, _)) => Err(Failure::Unauthorized),
        Err(ureq::Error::Status(code, response)) => {
            let text = response.into_string().unwrap_or_default();
            Err(Failure::Message(explain(code, &text)))
        }
        Err(e) => Err(Failure::Message(format!("could not reach Gmail: {e}"))),
    }
}

/// A multipart/mixed answer as the JSON objects in it. A part whose own status
/// line says 401 is the whole call's 401: the token is what every part used.
fn unbatch(content_type: &str, text: &str) -> Result<Vec<Value>, Failure> {
    let Some(separator) = separator(content_type, text) else {
        return Err(Failure::Message(
            "Gmail's batch answer did not say where one part ends".to_string(),
        ));
    };
    let parts: Vec<&str> = text.split(separator.as_str()).collect();
    if parts.iter().any(|part| refused(part)) {
        return Err(Failure::Unauthorized);
    }
    Ok(parts.iter().filter_map(|part| json_of(part)).collect())
}

/// What separates the parts: the boundary Gmail declared, or — failing that —
/// the first line of the body, which is that same boundary written out.
fn separator(content_type: &str, text: &str) -> Option<String> {
    if let Some(boundary) = boundary_of(content_type) {
        return Some(format!("--{boundary}"));
    }
    text.lines()
        .next()
        .map(str::trim_end)
        .filter(|line| line.starts_with("--"))
        .map(str::to_owned)
}

/// `multipart/mixed; boundary=batch_abc` → `batch_abc`.
fn boundary_of(content_type: &str) -> Option<String> {
    let at = content_type.to_ascii_lowercase().find("boundary=")?;
    let rest = content_type[at + "boundary=".len()..].trim();
    let value = rest.split(';').next().unwrap_or(rest).trim();
    let value = value.trim_matches('"');
    (!value.is_empty()).then(|| value.to_string())
}

/// Whether one part's inner response was a refused token.
fn refused(part: &str) -> bool {
    part.lines()
        .any(|line| line.starts_with("HTTP/") && line.contains(" 401"))
}

/// One part's JSON: past the part's own headers and past the inner response's,
/// which is where the first `{` is. What follows the object is the next
/// boundary's business, so trailing text is not an error here.
fn json_of(part: &str) -> Option<Value> {
    let start = part.find('{')?;
    serde_json::Deserializer::from_str(&part[start..])
        .into_iter::<Value>()
        .next()?
        .ok()
}

/// Google's own words about a refusal, which are usually the useful ones — a
/// disabled API and a missing scope both say exactly that.
pub fn explain(code: u16, body: &str) -> String {
    let said = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.pointer("/error_description"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.trim().chars().take(300).collect());
    if said.is_empty() {
        return format!("Gmail refused the call ({code})");
    }
    format!("Gmail refused the call ({code}): {said}")
}

// ------------------------------------------------------- the page on disk

/// The newest page as it was last fetched, if this computer has one. It is a
/// head start and nothing more: what comes back says `"cached": true`, and the
/// fresh page is already on its way.
fn cached_page(email: &str) -> Option<Value> {
    let text = std::fs::read_to_string(paths::list_cache_file(email)).ok()?;
    let page: Value = serde_json::from_str(&text).ok()?;
    // A page of an older shape is not this version's page.
    if page.get("v").and_then(Value::as_u64) != Some(CACHE) {
        return None;
    }
    // A file with no rows in it is not a page.
    page.get("messages").and_then(Value::as_array)?;
    Some(json!({
        "account": email,
        "messages": page.get("messages").cloned().unwrap_or_else(|| json!([])),
        "next_page": page.get("next_page").cloned().unwrap_or(Value::Null),
    }))
}

/// Keeps the page just fetched, and when it was fetched.
fn save_page(email: &str, answer: &Value) {
    let mut page = answer.clone();
    if let Some(row) = page.as_object_mut() {
        row.insert("fetched".to_string(), json!(store::now()));
        row.insert("v".to_string(), json!(CACHE));
    }
    write_page(email, &page);
}

fn write_page(email: &str, page: &Value) {
    let file = paths::list_cache_file(email);
    if let Ok(mut out) = store::create(&file) {
        use std::io::Write;
        let _ = out.write_all(page.to_string().as_bytes());
    }
}

/// Says whether an answer came off the disk or off the wire.
fn flagged(mut answer: Value, cached: bool) -> Value {
    if let Some(row) = answer.as_object_mut() {
        row.insert("cached".to_string(), json!(cached));
    }
    answer
}

/// Changes the cached page in place. A message read, marked or trashed is a
/// row that is now wrong, and a wrong row is not worth a refetch to fix.
fn amend_cache(email: &str, change: impl FnOnce(&mut Vec<Value>)) {
    let file = paths::list_cache_file(email);
    let Ok(text) = std::fs::read_to_string(&file) else {
        return;
    };
    let Ok(mut page) = serde_json::from_str::<Value>(&text) else {
        return;
    };
    let Some(rows) = page.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    change(rows);
    write_page(email, &page);
}

/// The cached row's unread flag, where there is a cached row.
fn cache_unread(email: &str, id: &str, unread: bool) {
    amend_cache(email, |rows| {
        for row in rows.iter_mut() {
            if row.get("id").and_then(Value::as_str) == Some(id)
                && let Some(row) = row.as_object_mut()
            {
                row.insert("unread".to_string(), json!(unread));
            }
        }
    });
}

/// A trashed message leaves the cached page, as it left the mailbox.
fn cache_drop(email: &str, id: &str) {
    amend_cache(email, |rows| {
        rows.retain(|row| row.get("id").and_then(Value::as_str) != Some(id));
    });
}

// --------------------------------------------------- the message on disk

/// A message already fetched, exactly as it was answered the first time. A
/// message is immutable, so this needs no expiry and no refresh behind it.
fn cached_message(email: &str, id: &str) -> Option<Value> {
    let text = std::fs::read_to_string(paths::read_cache_file(email, id)).ok()?;
    let mut message: Value = serde_json::from_str(&text).ok()?;
    // A file with no message in it is not a message.
    message.get("id").and_then(Value::as_str)?;
    // Its HTML is where bodies are kept now: a path saved before the state
    // dir moved names a file that is not there any more. A body that is gone
    // altogether is fetched again, rather than the message shown without it.
    if message.get("html_path").is_some_and(|path| !path.is_null()) {
        let file = paths::body_file(email, id);
        if !file.exists() {
            return None;
        }
        message["html_path"] = json!(file.display().to_string());
    }
    Some(message)
}

/// Keeps the message just fetched, once it is marked read — and only then. A
/// read whose mark did not go through is not the message's final shape: cached,
/// it would say `marked_read: false` forever, because nothing after a cache hit
/// ever calls Gmail again. It is not kept, so the next open marks it.
///
/// A body that could not be saved is not a failed read either way: the next
/// open simply fetches it again.
fn save_message(email: &str, id: &str, answer: &Value) {
    if answer.get("marked_read").and_then(Value::as_bool) != Some(true) {
        return;
    }
    if let Ok(mut out) = store::create(&paths::read_cache_file(email, id)) {
        use std::io::Write;
        let _ = out.write_all(answer.to_string().as_bytes());
    }
}

/// Forgets one cached message, for the two things that make it wrong: put back
/// to unread, or moved to the trash.
fn forget_message(email: &str, id: &str) {
    let _ = std::fs::remove_file(paths::read_cache_file(email, id));
}

// -------------------------------------------------- the refresh behind it

/// Who wants to hear about a page that refreshed itself.
fn listeners() -> &'static Mutex<Vec<Sender<Value>>> {
    static LISTENERS: OnceLock<Mutex<Vec<Sender<Value>>>> = OnceLock::new();
    LISTENERS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Asks to be told when a background refresh lands. A window subscribes once,
/// at start-up; every fresh page arrives on this channel in the same shape
/// [`list`] answers in, or as `{"account": …, "error": …}` when the refresh
/// itself failed — so nothing is left waiting for a page that is not coming.
///
/// Subscribing is also what turns the cache on: with nobody listening there is
/// nobody to hand a fresher page to, so [`list`] fetches instead.
pub fn subscribe() -> Receiver<Value> {
    let (tx, rx) = channel();
    if let Ok(mut listeners) = listeners().lock() {
        listeners.push(tx);
    }
    rx
}

fn watched() -> bool {
    listeners().lock().is_ok_and(|listeners| !listeners.is_empty())
}

/// Hands a fresh page to everyone still listening, and forgets the rest.
fn publish(answer: &Value) {
    if let Ok(mut listeners) = listeners().lock() {
        listeners.retain(|tx| tx.send(answer.clone()).is_ok());
    }
}

/// The accounts a refresh is already running for.
fn refreshing() -> &'static Mutex<HashSet<String>> {
    static REFRESHING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    REFRESHING.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Fetches the newest page on a thread of its own and publishes it. One at a
/// time per account: listing an inbox twice in a second is one refresh.
fn refresh_in_background(email: &str) {
    let Ok(mut running) = refreshing().lock() else {
        return;
    };
    if !running.insert(email.to_string()) {
        return;
    }
    drop(running);
    let account = email.to_string();
    let started = std::thread::Builder::new()
        .name("post-refresh".into())
        .spawn(move || {
            let answer = match fetch(&account, None) {
                Ok(answer) => {
                    save_page(&account, &answer);
                    flagged(answer, false)
                }
                Err(error) => json!({"account": account, "error": error}),
            };
            if let Ok(mut running) = refreshing().lock() {
                running.remove(&account);
            }
            publish(&answer);
        });
    if started.is_err()
        && let Ok(mut running) = refreshing().lock()
    {
        running.remove(email);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A metadata-format message, exactly as the listing fetches them.
    fn metadata() -> Value {
        json!({
            "id": "18f3a2c9b1",
            "threadId": "18f3a2c9b1",
            "labelIds": ["UNREAD", "INBOX"],
            "internalDate": "1757673180000",
            "payload": {
                "mimeType": "multipart/alternative",
                "headers": [
                    {"name": "Subject", "value": "Re: the quote"},
                    {"name": "From", "value": "Ale <ale@acme.com>"},
                    {"name": "Date", "value": "Fri, 12 Sep 2025 12:33:00 +0200"}
                ]
            }
        })
    }

    #[test]
    fn a_listing_row_is_who_what_when_and_unread() {
        let row = summary(&metadata());
        assert_eq!(row["id"], "18f3a2c9b1");
        // Which conversation it is on, so a page of rows groups itself.
        assert_eq!(row["thread_id"], "18f3a2c9b1");
        assert_eq!(row["from"], "Ale <ale@acme.com>");
        assert_eq!(row["subject"], "Re: the quote");
        assert_eq!(row["date"], "2025-09-12 10:33");
        assert_eq!(row["unread"], true);
    }

    #[test]
    fn a_read_message_says_so_and_a_bare_one_does_not_panic() {
        let mut read = metadata();
        read["labelIds"] = json!(["INBOX"]);
        assert_eq!(summary(&read)["unread"], false);

        let bare = json!({"id": "abc"});
        let row = summary(&bare);
        assert_eq!(row["id"], "abc");
        assert_eq!(row["thread_id"], "");
        assert_eq!(row["unread"], false);
        assert_eq!(row["subject"], "");
        assert_eq!(row["date"], "");
    }

    /// With no timestamp of Gmail's own, the sender's header stands in.
    #[test]
    fn the_date_header_is_the_fallback() {
        let mut message = metadata();
        message.as_object_mut().expect("object").remove("internalDate");
        assert_eq!(summary(&message)["date"], "Fri, 12 Sep 2025 12:33:00 +0200");
    }

    #[test]
    fn an_id_that_is_not_an_id_is_refused_before_it_reaches_a_url() {
        assert_eq!(checked_id(" 18f3a2c9b1 ").expect("an id"), "18f3a2c9b1");
        for bad in ["", "../../profile", "a b", "18f3?format=full", "x/y"] {
            let e = checked_id(bad).expect_err("not an id");
            assert!(e.contains("not a message id"), "got {e}");
        }
    }

    // ------------------------------------------------------- the batch call

    /// A multipart answer in the shape Gmail sends one: its own boundary, one
    /// part per inner call, and each part carrying its own status line and
    /// headers above the JSON.
    fn batch_answer(boundary: &str, parts: &[(u16, &str)]) -> String {
        let mut text = String::new();
        for (code, body) in parts {
            let said = if *code == 200 { "OK" } else { "Unauthorized" };
            text.push_str(&format!(
                "--{boundary}\r\n\
                 Content-Type: application/http\r\n\
                 Content-ID: response-\r\n\r\n\
                 HTTP/1.1 {code} {said}\r\n\
                 Content-Type: application/json; charset=UTF-8\r\n\r\n\
                 {body}\r\n\r\n"
            ));
        }
        text.push_str(&format!("--{boundary}--\r\n"));
        text
    }

    /// One call up, N answers down: the batch answer is the JSON out of its
    /// parts, in the order Gmail wrote them.
    #[test]
    fn a_batch_answer_is_the_json_in_its_parts() {
        let body = batch_answer(
            "batch_abc123",
            &[
                (200, r#"{"id":"18f3","labelIds":["UNREAD","INBOX"]}"#),
                (200, r#"{"id":"18f2","labelIds":["INBOX"]}"#),
                (200, r#"{"id":"18f1","labelIds":[]}"#),
            ],
        );
        let parsed = match unbatch("multipart/mixed; boundary=batch_abc123", &body) {
            Ok(parsed) => parsed,
            Err(e) => panic!("{}", e.message()),
        };
        let ids: Vec<&str> = parsed
            .iter()
            .map(|m| m.get("id").and_then(Value::as_str).expect("an id"))
            .collect();
        assert_eq!(ids, vec!["18f3", "18f2", "18f1"]);
        // Whole objects, not text: the labels are there to read.
        assert_eq!(parsed[0]["labelIds"][0], "UNREAD");
        assert_eq!(summary(&parsed[0])["unread"], true);
        assert_eq!(summary(&parsed[1])["unread"], false);
    }

    /// Where one part ends: the boundary Gmail declared in its header, and —
    /// when it declared none — the first line of the body, which is that same
    /// boundary written out.
    #[test]
    fn the_boundary_is_gmails_own_and_the_first_line_stands_in_for_it() {
        assert_eq!(
            boundary_of("multipart/mixed; boundary=batch_abc").as_deref(),
            Some("batch_abc")
        );
        assert_eq!(
            boundary_of("multipart/mixed; BOUNDARY=\"batch_abc\"; charset=utf-8").as_deref(),
            Some("batch_abc")
        );
        assert_eq!(boundary_of("application/json"), None);
        assert_eq!(boundary_of("multipart/mixed; boundary="), None);

        let body = batch_answer("batch_xyz", &[(200, r#"{"id":"18f3"}"#)]);
        assert_eq!(
            separator("multipart/mixed; boundary=batch_xyz", &body).as_deref(),
            Some("--batch_xyz")
        );
        assert_eq!(
            separator("multipart/mixed", &body).as_deref(),
            Some("--batch_xyz"),
            "the body says it too"
        );
        assert_eq!(separator("multipart/mixed", "{\"id\":\"18f3\"}"), None);

        // And with neither, the page is not a page: say so rather than hand
        // back an empty listing that looks like an empty mailbox.
        let e = match unbatch("application/json", "{\"id\":\"18f3\"}") {
            Err(e) => e.message(),
            Ok(parsed) => panic!("a shapeless answer became {parsed:?}"),
        };
        assert!(e.contains("where one part ends"), "got {e}");
    }

    /// The token is what every part used, so one part refusing it is the whole
    /// call refusing it — which is the one thing worth a fresh token and a
    /// second try.
    #[test]
    fn one_part_refusing_the_token_refuses_the_whole_call() {
        let refused_one = batch_answer(
            "batch_abc",
            &[
                (200, r#"{"id":"18f3"}"#),
                (401, r#"{"error":{"code":401,"message":"Invalid Credentials"}}"#),
            ],
        );
        assert!(matches!(
            unbatch("multipart/mixed; boundary=batch_abc", &refused_one),
            Err(Failure::Unauthorized)
        ));

        // Every part answered: nothing to retry, and the page is the page.
        let answered = batch_answer(
            "batch_abc",
            &[(200, r#"{"id":"18f3"}"#), (200, r#"{"id":"18f2"}"#)],
        );
        assert!(matches!(
            unbatch("multipart/mixed; boundary=batch_abc", &answered),
            Ok(parsed) if parsed.len() == 2
        ));
    }

    /// A refusal is a part's own status line and nothing else. A message that
    /// merely says the number is a message, not a refused token.
    #[test]
    fn only_an_inner_status_line_is_a_refusal() {
        assert!(refused("HTTP/1.1 401 Unauthorized\r\n\r\n{}"));
        assert!(!refused("HTTP/1.1 200 OK\r\n\r\n{\"id\":\"18f3\"}"));
        assert!(
            !refused("HTTP/1.1 200 OK\r\n\r\n{\"subject\":\"invoice 401 is due\"}"),
            "a subject line is not a status line"
        );
        assert!(!refused("HTTP/1.1 404 Not Found\r\n\r\n{}"));
        assert!(!refused(""));
    }

    /// One part is the first object in it: past the part's own headers, and
    /// past the inner response's. What follows the object is the next
    /// boundary's business.
    #[test]
    fn a_part_is_the_first_object_in_it_and_nothing_after() {
        let part = "\r\nContent-Type: application/http\r\n\r\n\
                    HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
                    {\"id\":\"18f3\"}\r\n";
        assert_eq!(json_of(part).expect("an object")["id"], "18f3");
        assert_eq!(
            json_of("{\"id\":\"18f3\"}\r\n--batch_abc\r\nContent-Type: x\r\n")
                .expect("an object")["id"],
            "18f3"
        );

        // The preamble and the epilogue of a multipart body carry no JSON, and
        // neither does a part that came back as anything but JSON.
        assert_eq!(json_of("\r\n"), None);
        assert_eq!(json_of("--\r\n"), None);
        assert_eq!(json_of("HTTP/1.1 500 Internal Error\r\n\r\n<html>oops</html>"), None);
        // An inner error is an object, but not a message: it carries no id, so
        // the page simply does not have that row.
        let error = json_of("HTTP/1.1 404 Not Found\r\n\r\n{\"error\":{\"code\":404}}")
            .expect("an object");
        assert!(error.get("id").is_none());
    }

    // ---------------------------------------------------- the page on disk

    /// A page exactly as a fetch builds one.
    fn page(email: &str, ids: &[&str], next: Option<&str>) -> Value {
        let messages: Vec<Value> = ids
            .iter()
            .map(|id| {
                json!({"id": id, "from": "Ale <ale@acme.com>", "subject": "Re: the quote",
                       "date": "2025-09-12 10:33", "unread": true})
            })
            .collect();
        json!({"account": email, "messages": messages, "next_page": next})
    }

    /// Writes `text` where a cached page or a cached message would be, which is
    /// the only way to have a file there that is not one.
    fn put(file: std::path::PathBuf, text: &str) {
        use std::io::Write;
        let mut out = store::create(&file).expect("a file");
        out.write_all(text.as_bytes()).expect("written");
    }

    /// The newest page is kept so a window has mail to draw before the network
    /// has answered, and it comes back as the page it was.
    #[test]
    fn the_newest_page_is_kept_and_handed_back() {
        paths::with_state_dir(|_| {
            let email = "kept@acme.com";
            assert_eq!(cached_page(email), None, "a fresh computer has no page");

            save_page(email, &page(email, &["18f3", "18f2"], Some("tok3n")));
            let cached = cached_page(email).expect("a page");
            assert_eq!(cached["account"], email);
            assert_eq!(cached["messages"].as_array().expect("rows").len(), 2);
            assert_eq!(cached["messages"][0]["id"], "18f3");
            assert_eq!(cached["next_page"], "tok3n");

            // The file says when it was fetched; the page handed over does not
            // — that is the cache's own bookkeeping, not a row.
            let file = std::fs::read_to_string(paths::list_cache_file(email)).expect("the file");
            let on_disk: Value = serde_json::from_str(&file).expect("json");
            assert!(on_disk["fetched"].as_u64().expect("a stamp") > 0);
            assert!(cached.get("fetched").is_none());
        });
    }

    /// A file with no rows in it is not a page: it is fetched instead, rather
    /// than drawn as an empty mailbox.
    #[test]
    fn a_file_that_is_not_a_page_is_not_used_as_one() {
        paths::with_state_dir(|_| {
            let email = "junk@acme.com";
            for text in ["", "not json at all", "{}", "[]", "{\"messages\": null}"] {
                put(paths::list_cache_file(email), text);
                assert_eq!(cached_page(email), None, "\"{text}\" is not a page");
            }
        });
    }

    /// A row that is now wrong is corrected where it sits: one flag is not
    /// worth a refetch, and a row that lies is worse than one a second old.
    #[test]
    fn marking_a_message_amends_the_cached_row() {
        paths::with_state_dir(|_| {
            let email = "amend@acme.com";
            save_page(email, &page(email, &["18f3", "18f2"], Some("tok3n")));

            cache_unread(email, "18f3", false);
            let cached = cached_page(email).expect("a page");
            assert_eq!(cached["messages"][0]["unread"], false);
            assert_eq!(cached["messages"][1]["unread"], true, "and only that row");
            assert_eq!(cached["next_page"], "tok3n", "the rest of the page is untouched");

            // And back again, for a message put back to unread.
            cache_unread(email, "18f3", true);
            assert_eq!(
                cached_page(email).expect("a page")["messages"][0]["unread"],
                true
            );

            // A message this page has never heard of changes nothing.
            cache_unread(email, "nope", false);
            let rows = cached_page(email).expect("a page");
            assert_eq!(rows["messages"].as_array().expect("rows").len(), 2);
        });
    }

    /// A trashed message leaves the cached page, as it left the mailbox.
    #[test]
    fn a_trashed_message_leaves_the_cached_page() {
        paths::with_state_dir(|_| {
            let email = "drop@acme.com";
            save_page(email, &page(email, &["18f3", "18f2"], None));

            cache_drop(email, "18f2");
            let cached = cached_page(email).expect("a page");
            let ids: Vec<&str> = cached["messages"]
                .as_array()
                .expect("rows")
                .iter()
                .map(|row| row["id"].as_str().expect("an id"))
                .collect();
            assert_eq!(ids, vec!["18f3"]);

            // Trashing it twice is not an error, and does not empty the page.
            cache_drop(email, "18f2");
            let cached = cached_page(email).expect("a page");
            assert_eq!(cached["messages"].as_array().expect("rows").len(), 1);
        });
    }

    /// A computer with no cached page has nothing to amend — and amending it
    /// does not conjure one.
    #[test]
    fn amending_a_page_this_computer_does_not_have_is_not_an_error() {
        paths::with_state_dir(|_| {
            let email = "nothing@acme.com";
            cache_unread(email, "18f3", false);
            cache_drop(email, "18f3");
            assert!(!paths::list_cache_file(email).exists());
            assert_eq!(cached_page(email), None);
        });
    }

    /// Every page says whether it came off the disk or off the wire, so a
    /// window knows a fresher one is on its way.
    #[test]
    fn a_page_says_whether_it_came_off_the_disk() {
        assert_eq!(flagged(json!({"messages": []}), true)["cached"], true);
        assert_eq!(flagged(json!({"messages": []}), false)["cached"], false);
        // Nothing to flag is not a panic.
        assert_eq!(flagged(json!([]), true), json!([]));
    }

    // ------------------------------------------------- the message on disk

    /// A message does not change, so a message read once is read from disk
    /// every time after: no call to Gmail, and no second mark — it was marked
    /// the first time.
    #[test]
    fn a_message_read_once_is_read_from_disk() {
        paths::with_state_dir(|_| {
            let email = "read@acme.com";
            let answer = json!({
                "id": "18f3a2c9b1",
                "from": "Ale <ale@acme.com>",
                "subject": "Re: the quote",
                "date": "2025-09-12 10:33",
                "text": "Looks good — send it.",
                "html_path": Value::Null,
                "attachments": [],
                "unread": false,
                "marked_read": true,
            });
            save_message(email, "18f3a2c9b1", &answer);

            // No account is connected under this state dir, so anything that
            // went near the network would come back as an error instead.
            let again = read(email, " 18f3a2c9b1 ").expect("the message, off the disk");
            assert_eq!(again, answer);
            assert_eq!(again["marked_read"], true, "marked once, on the first read");
            assert_eq!(again["unread"], false);
        });
    }

    /// A read whose mark did not go through is not kept: nothing calls Gmail
    /// again after a cache hit, so a cached `marked_read: false` would be
    /// frozen there. The next open fetches the message and marks it instead.
    #[test]
    fn a_message_that_could_not_be_marked_read_is_not_kept() {
        paths::with_state_dir(|_| {
            let email = "unmarked@acme.com";
            let unmarked = json!({
                "id": "18f3", "text": "hi", "unread": true, "marked_read": false,
            });
            save_message(email, "18f3", &unmarked);
            assert!(!paths::read_cache_file(email, "18f3").exists());
            assert_eq!(cached_message(email, "18f3"), None);

            // A message with nothing to say about the flag is not kept either.
            save_message(email, "18f3", &json!({"id": "18f3", "text": "hi"}));
            assert_eq!(cached_message(email, "18f3"), None);

            // Marked read, and it is kept — that is the whole condition.
            let marked = json!({
                "id": "18f3", "text": "hi", "unread": false, "marked_read": true,
            });
            save_message(email, "18f3", &marked);
            assert_eq!(cached_message(email, "18f3"), Some(marked));
        });
    }

    /// A message kept before its body moved points at the body where it is
    /// now; one whose body is gone is fetched again.
    #[test]
    fn a_kept_message_finds_its_body_where_it_is_now() {
        paths::with_state_dir(|_| {
            let email = "moved@acme.com";
            let kept = json!({
                "id": "18f3", "text": "hi", "unread": false, "marked_read": true,
                "html_path": "/home/ed/.local/share/elsewhere/postbodies/moved@acme.com/18f3.html",
            });
            save_message(email, "18f3", &kept);
            assert_eq!(cached_message(email, "18f3"), None, "no body anywhere: fetched again");

            let body = paths::body_file(email, "18f3");
            put(body.clone(), "<p>hi</p>");
            let found = cached_message(email, "18f3").expect("kept");
            assert_eq!(found["html_path"], json!(body.display().to_string()));

            // A message without HTML has no body to find.
            let plain = json!({"id": "18f4", "text": "hi", "marked_read": true, "html_path": null});
            save_message(email, "18f4", &plain);
            assert_eq!(cached_message(email, "18f4"), Some(plain));
        });
    }

    /// A file with no message in it is not a message: the next open fetches.
    #[test]
    fn a_file_that_is_not_a_message_is_fetched_again() {
        paths::with_state_dir(|_| {
            let email = "junkread@acme.com";
            for text in ["", "not json at all", "{}", "{\"text\": \"hi\"}"] {
                put(paths::read_cache_file(email, "18f3"), text);
                assert!(
                    cached_message(email, "18f3").is_none(),
                    "\"{text}\" is not a message"
                );
            }
        });
    }

    /// The two things that make a cached message wrong — put back to unread,
    /// and moved to the trash — forget it, so the next open fetches it again.
    #[test]
    fn forgetting_a_message_removes_its_file() {
        paths::with_state_dir(|_| {
            let email = "forget@acme.com";
            save_message(
                email,
                "18f3",
                &json!({"id": "18f3", "text": "hi", "marked_read": true}),
            );
            assert!(paths::read_cache_file(email, "18f3").exists());
            assert!(cached_message(email, "18f3").is_some());

            forget_message(email, "18f3");
            assert!(!paths::read_cache_file(email, "18f3").exists());
            assert!(cached_message(email, "18f3").is_none());

            // Forgetting one that is not there is not an error.
            forget_message(email, "18f3");
        });
    }

    #[test]
    fn googles_own_words_come_through() {
        let body = json!({"error": {"code": 403, "message":
            "Gmail API has not been used in project 42 before or it is disabled."}})
        .to_string();
        let said = explain(403, &body);
        assert!(said.contains("403"), "got {said}");
        assert!(said.contains("has not been used"), "got {said}");
        // Not JSON at all, and empty: still one sentence, never a panic.
        assert!(explain(500, "<html>oops</html>").contains("500"));
        assert!(explain(500, "").contains("500"));
    }
}
