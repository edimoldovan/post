//! Connecting an account: OAuth code+PKCE, in the system browser.
//!
//! Google's own consent screen is the confirmation — the person sees which
//! account and which permission, on Google's page, in their own browser. Post
//! never sees a password, and there is no dialog of ours in the way.
//!
//! The flow is hand-rolled because it is small: a random verifier, its SHA-256
//! challenge, a listener on a loopback port, one redirect back with a code, and
//! one POST to exchange it. The redirect URI is `http://127.0.0.1:<port>` with
//! an ephemeral port, which is exactly what a Desktop OAuth client allows.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::store::{Client, OauthTokens};
use crate::{gmail, paths, store};

/// What Post asks for: read mail and change its labels, and send a reply.
///
/// Two scopes, because `gmail.modify` does not cover sending — Google keeps
/// `users.messages.send` behind `gmail.send`, which grants that and nothing
/// else. An account connected before this line grew has only the first, and
/// has to be connected again for `post_send` to work.
pub const SCOPE: &str = "https://www.googleapis.com/auth/gmail.modify \
                         https://www.googleapis.com/auth/gmail.send";

const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// How long the browser has. Long enough to find the right Google account,
/// short enough that a forgotten tab does not hold a tool call forever.
pub const WAIT: Duration = Duration::from_secs(300);

/// Connects one account, start to finish, and returns its address.
///
/// It blocks: the listener is up, the browser is open, and the person is at
/// Google. That is the whole point of the verb, and the caller — a tool call or
/// a terminal — waits with it.
pub fn add_account() -> Result<String, String> {
    // The client first: with no client there is nothing to open a browser for,
    // and the error is the one thing to act on.
    let client = store::client()?;

    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("could not listen on a loopback port: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("could not read the loopback port: {e}"))?
        .port();
    let redirect = format!("http://127.0.0.1:{port}");

    // Two random strings: one proves this process asked for the code, the other
    // that the redirect belongs to this attempt.
    let verifier = verifier()?;
    let state = random()?;
    let url = auth_url(&client.client_id, &redirect, &challenge(&verifier), &state);
    open_browser(&url);

    let code = wait_for_code(&listener, &state, WAIT)?;
    let tokens = exchange(&client, &code, &verifier, &redirect)?;
    if tokens.refresh_token.is_empty() {
        return Err("Google sent no refresh token, so the account would not stay \
                    connected — remove Post's access at \
                    https://myaccount.google.com/permissions and try again"
            .to_string());
    }

    let email = gmail::profile(&tokens.access_token)?;
    store::save_oauth_tokens(&paths::oauth_tokens_dir(), &email, &tokens)?;
    store::add_account(&paths::accounts_file(), &email)?;
    Ok(email)
}

/// A new access token from the refresh token. Called for the caller, never by
/// them.
/// Why a refresh failed: the grant itself is dead — only a new sign-in
/// helps — or something else went wrong.
pub enum RefreshError {
    /// The token endpoint answered 400/401: the grant is expired or revoked.
    Reauth,
    Other(String),
}

pub fn refresh(client: &Client, refresh_token: &str) -> Result<OauthTokens, RefreshError> {
    match form_status(&[
        ("client_id", client.client_id.as_str()),
        ("client_secret", client.client_secret.as_str()),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ]) {
        Ok(answer) => Ok(oauth_tokens_from(&answer, refresh_token)),
        Err((Some(400 | 401), _)) => Err(RefreshError::Reauth),
        Err((_, text)) => Err(RefreshError::Other(text)),
    }
}

/// The authorization code for OAuth tokens.
fn exchange(
    client: &Client,
    code: &str,
    verifier: &str,
    redirect: &str,
) -> Result<OauthTokens, String> {
    let answer = form(&[
        ("client_id", client.client_id.as_str()),
        ("client_secret", client.client_secret.as_str()),
        ("code", code),
        ("code_verifier", verifier),
        ("grant_type", "authorization_code"),
        ("redirect_uri", redirect),
    ])?;
    Ok(oauth_tokens_from(&answer, ""))
}

/// One POST to Google's token endpoint, keeping the HTTP status so a caller
/// can decide by the code rather than by the message's wording.
fn form_status(fields: &[(&str, &str)]) -> Result<Value, (Option<u16>, String)> {
    match gmail::agent().post(TOKEN_ENDPOINT).send_form(fields) {
        Ok(response) => {
            let text = response
                .into_string()
                .map_err(|e| (None, format!("Google's answer stopped short: {e}")))?;
            serde_json::from_str(&text)
                .map_err(|e| (None, format!("Google sent something unreadable: {e}")))
        }
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            Err((Some(code), gmail::explain(code, &body)))
        }
        Err(e) => Err((None, format!("could not reach Google: {e}"))),
    }
}

/// One POST to Google's token endpoint, as a form.
fn form(fields: &[(&str, &str)]) -> Result<Value, String> {
    match gmail::agent().post(TOKEN_ENDPOINT).send_form(fields) {
        Ok(response) => {
            let text = response
                .into_string()
                .map_err(|e| format!("Google's answer stopped short: {e}"))?;
            serde_json::from_str(&text)
                .map_err(|e| format!("Google sent something unreadable: {e}"))
        }
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            Err(gmail::explain(code, &body))
        }
        Err(e) => Err(format!("could not reach Google: {e}")),
    }
}

/// Google's token JSON as our OAuth tokens. `fallback` keeps a refresh token
/// Google did not resend.
fn oauth_tokens_from(answer: &Value, fallback: &str) -> OauthTokens {
    let text = |key: &str| {
        answer
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let lifetime = answer
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(3600);
    let refresh_token = match text("refresh_token") {
        empty if empty.is_empty() => fallback.to_string(),
        given => given,
    };
    OauthTokens {
        refresh_token,
        access_token: text("access_token"),
        expiry: store::now() + lifetime,
    }
}

/// The URL the browser opens.
///
/// `access_type=offline` with `prompt=consent` is what makes Google hand over a
/// refresh token: without it the account would fall out after an hour.
pub fn auth_url(client_id: &str, redirect: &str, challenge: &str, state: &str) -> String {
    format!(
        "{AUTH_ENDPOINT}?client_id={}&redirect_uri={}&response_type=code&scope={}\
         &code_challenge={}&code_challenge_method=S256&state={}\
         &access_type=offline&prompt=consent",
        urlencode(client_id),
        urlencode(redirect),
        urlencode(SCOPE),
        urlencode(challenge),
        urlencode(state),
    )
}

/// 32 random bytes, base64url: 43 characters of the PKCE alphabet.
pub fn random() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("no randomness on this computer: {e}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// The PKCE verifier: a fresh random string, never reused.
pub fn verifier() -> Result<String, String> {
    random()
}

/// The challenge for a verifier: base64url of its SHA-256, per PKCE's S256.
pub fn challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// Opens the system browser, and says where to go in case it does not.
fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    match Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        // Reaped on a thread of its own: the opener exits at once, and nothing
        // is left behind either way.
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => eprintln!("post: could not open a browser ({e})"),
    }
    eprintln!("post: sign in to Google at {url}");
}

/// Waits for Google to redirect back with a code, answering the browser with a
/// page that says it worked.
fn wait_for_code(listener: &TcpListener, state: &str, wait: Duration) -> Result<String, String> {
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("could not wait on the loopback port: {e}"))?;
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((stream, _)) => match redirect_of(stream, state) {
                // Something else knocked — a favicon, a stray tab. Keep waiting
                // for the real one.
                Ok(None) => continue,
                Ok(Some(code)) => return Ok(code),
                Err(e) => return Err(e),
            },
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(e) => return Err(format!("the loopback port failed: {e}")),
        }
    }
    Err(format!(
        "nothing came back from Google within {} minutes — nothing was connected",
        wait.as_secs() / 60
    ))
}

/// Reads one HTTP request off the loopback socket and answers it. `Ok(None)`
/// means it was not the redirect.
fn redirect_of(mut stream: TcpStream, state: &str) -> Result<Option<String>, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| format!("the browser's connection failed: {e}"))?;

    // The request line is all that matters, and it is short. Read a page of
    // bytes and take the first line of it.
    let mut buffer = [0u8; 8192];
    let read = stream.read(&mut buffer).unwrap_or(0);
    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("")
        .to_string();
    let fields = query(&target);
    let field = |key: &str| {
        fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };

    if let Some(error) = field("error") {
        answer(&mut stream, "Post was not connected.", &error);
        return Err(format!("Google said no: {error}"));
    }
    let Some(code) = field("code") else {
        let mut nothing = stream;
        let _ = nothing.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return Ok(None);
    };
    // The state is ours; if it does not come back, this is not our redirect.
    if field("state").as_deref() != Some(state) {
        answer(&mut stream, "Post was not connected.", "");
        return Err(
            "the sign-in came back with the wrong state — nothing was connected".to_string(),
        );
    }
    answer(
        &mut stream,
        "Post is connected.",
        "You can close this tab.",
    );
    Ok(Some(code))
}

/// The one page the browser ever gets from us.
fn answer(stream: &mut TcpStream, headline: &str, detail: &str) {
    let page = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <title>Post</title></head>\
         <body style=\"font:16px system-ui;margin:4rem auto;max-width:32rem\">\
         <h1 style=\"font-size:1.25rem\">{headline}</h1><p>{detail}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// The query of a request target, percent-decoded.
pub fn query(target: &str) -> Vec<(String, String)> {
    let Some((_, query)) = target.split_once('?') else {
        return Vec::new();
    };
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => (percent_decode(key), percent_decode(value)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

/// Percent-decoding, plus `+` for a space: what a browser sends back.
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&text[i + 1..i + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    // Not an escape after all; keep the character.
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encoding for a query value: unreserved characters stay, everything
/// else goes as %XX.
pub fn urlencode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636's own S256 example, so the pair is right by the book rather
    /// than by agreement with itself.
    #[test]
    fn the_pkce_challenge_matches_the_specs_vector() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_verifier_is_long_random_and_url_safe() {
        let one = verifier().expect("randomness");
        let two = verifier().expect("randomness");
        assert_ne!(one, two, "two verifiers must not be the same");
        // 32 bytes, base64url, unpadded.
        assert_eq!(one.len(), 43);
        assert!(
            one.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "got {one}"
        );
        // And it is a legal PKCE verifier, which is 43..=128 characters.
        assert!((43..=128).contains(&one.len()));
    }

    #[test]
    fn the_auth_url_carries_the_scope_the_challenge_and_the_loopback() {
        let url = auth_url("42.apps.googleusercontent.com", "http://127.0.0.1:39411", "chal", "st8");
        assert!(url.starts_with(AUTH_ENDPOINT), "got {url}");
        assert!(url.contains("code_challenge=chal&code_challenge_method=S256"), "got {url}");
        assert!(url.contains("response_type=code"), "got {url}");
        assert!(url.contains("access_type=offline"), "got {url}");
        assert!(url.contains("prompt=consent"), "got {url}");
        assert!(url.contains("state=st8"), "got {url}");
        assert!(
            url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A39411"),
            "got {url}"
        );
        assert!(
            url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fgmail.modify"),
            "got {url}"
        );
        // Sending is its own scope: gmail.modify does not cover it.
        assert!(
            url.contains("%20https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fgmail.send"),
            "got {url}"
        );
        assert_eq!(SCOPE.split_whitespace().count(), 2, "two scopes, one space");
    }

    #[test]
    fn the_redirects_query_is_read_back() {
        let fields = query("/?state=st8&code=4%2F0AVG7fiQ-abc&scope=https%3A%2F%2Fmail");
        assert_eq!(fields[0], ("state".to_string(), "st8".to_string()));
        assert_eq!(fields[1], ("code".to_string(), "4/0AVG7fiQ-abc".to_string()));
        assert_eq!(fields[2].1, "https://mail");
        // Google's other answer.
        let denied = query("/?error=access_denied&state=st8");
        assert_eq!(denied[0], ("error".to_string(), "access_denied".to_string()));
        // Nothing to read is nothing, not a panic.
        assert!(query("/").is_empty());
        assert!(query("").is_empty());
    }

    #[test]
    fn percent_decoding_survives_what_a_browser_sends() {
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("one+two"), "one two");
        assert_eq!(percent_decode("100%25"), "100%");
        assert_eq!(percent_decode("%E2%82%AC"), "€");
        // A stray percent is a percent.
        assert_eq!(percent_decode("50%"), "50%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn encoding_and_decoding_are_each_others_undoing() {
        for text in ["ed@acme.com", "a b/c?d=e&f", "€ 100", "plain"] {
            assert_eq!(percent_decode(&urlencode(text)), text);
        }
    }

    /// Google's token answer, and the one it sends on a refresh — which has no
    /// refresh token in it.
    #[test]
    fn a_token_answer_becomes_oauth_tokens_and_a_refresh_keeps_the_old_one() {
        let first = serde_json::json!({
            "access_token": "ya29.first",
            "expires_in": 3599,
            "refresh_token": "1//refresh",
            "scope": SCOPE,
            "token_type": "Bearer",
        });
        let tokens = oauth_tokens_from(&first, "");
        assert_eq!(tokens.access_token, "ya29.first");
        assert_eq!(tokens.refresh_token, "1//refresh");
        assert!(tokens.expiry > store::now() + 3500);

        let refreshed = serde_json::json!({
            "access_token": "ya29.second",
            "expires_in": 3599,
            "token_type": "Bearer",
        });
        let tokens = oauth_tokens_from(&refreshed, "1//refresh");
        assert_eq!(tokens.access_token, "ya29.second");
        assert_eq!(
            tokens.refresh_token, "1//refresh",
            "Google does not resend it, so it must be kept"
        );
    }

    /// The wait has to end by itself: a person who never finishes at Google
    /// leaves a tool call hanging otherwise.
    #[test]
    fn waiting_for_a_code_gives_up() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let e = wait_for_code(&listener, "st8", Duration::from_millis(300))
            .expect_err("nothing came back");
        assert!(e.contains("nothing was connected"), "got {e}");
    }

    /// The browser's redirect, over a real loopback socket, with no Google in
    /// it: the code comes out and the tab gets a page.
    #[test]
    fn a_redirect_hands_over_its_code() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("addr").port();
        let browser = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
            let _ = stream.write_all(
                b"GET /?state=st8&code=4%2Fcode HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            );
            let mut page = String::new();
            let _ = stream.read_to_string(&mut page);
            page
        });
        let code = wait_for_code(&listener, "st8", Duration::from_secs(5)).expect("a code");
        assert_eq!(code, "4/code");
        let page = browser.join().expect("the browser thread");
        assert!(page.contains("Post is connected"), "got {page}");
    }

    /// A redirect with someone else's state is not ours.
    #[test]
    fn a_wrong_state_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let port = listener.local_addr().expect("addr").port();
        let browser = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
            let _ = stream.write_all(b"GET /?state=other&code=x HTTP/1.1\r\n\r\n");
            let mut page = String::new();
            let _ = stream.read_to_string(&mut page);
        });
        let e = wait_for_code(&listener, "st8", Duration::from_secs(5)).expect_err("refused");
        assert!(e.contains("wrong state"), "got {e}");
        browser.join().expect("the browser thread");
    }
}
