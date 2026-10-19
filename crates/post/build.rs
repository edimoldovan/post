//! Embeds the Google OAuth client in the binary.
//!
//! The client id and secret are application config, not a person's: one
//! Desktop client from the Google Cloud console belongs to Post itself, and
//! every copy signs in with it. So it is baked in here rather than asked of
//! whoever installs the app.
//!
//! Two sources, in order: git-ignored `config/oauth.json` at the top of the
//! repository, then the environment (`POST_GOOGLE_CLIENT_ID` /
//! `POST_GOOGLE_CLIENT_SECRET`) for a build machine that keeps its secrets
//! out of the tree. With neither, the build still succeeds and the crate says
//! what to put where at the first op that needs a client.
//!
//! The file is keyed by provider —
//! `{"google":{"client_id":…,"client_secret":…}}` — so Microsoft and the rest
//! arrive as sibling keys. This build script reads the `"google"` object.

use std::path::{Path, PathBuf};

const FILE: &str = "oauth.json";
const PROVIDER: &str = "google";
const ID: &str = "POST_GOOGLE_CLIENT_ID";
const SECRET: &str = "POST_GOOGLE_CLIENT_SECRET";

fn main() {
    let file = oauth_file();
    println!("cargo:rerun-if-changed={}", file.display());
    println!("cargo:rerun-if-env-changed={ID}");
    println!("cargo:rerun-if-env-changed={SECRET}");

    let (id, secret) = from_file(&file).unwrap_or_else(from_env);
    println!("cargo:rustc-env={ID}={}", one_line(&id));
    println!("cargo:rustc-env={SECRET}={}", one_line(&secret));
}

/// `config/oauth.json` beside the workspace root — two levels up from
/// `crates/post`, which is where this crate sits.
fn oauth_file() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    Path::new(&manifest)
        .join("..")
        .join("..")
        .join("config")
        .join(FILE)
}

/// The pair out of the oauth file's `"google"` object, or nothing if the file
/// is not there or that object is missing either field. Hand-parsed: a build
/// script with no dependencies of its own.
fn from_file(file: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(file).ok()?;
    let provider = object(&text, PROVIDER)?;
    let id = field(provider, "client_id")?;
    let secret = field(provider, "client_secret")?;
    Some((id, secret))
}

/// The body of one provider's object: from the `{` after the key to the next
/// `}`. A provider holds string fields only, and ids and secrets carry no
/// braces, so the first `}` ends it.
fn object<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let after = text.split_once(&format!("\"{key}\""))?.1;
    let after = after.split_once('{')?.1;
    let (body, _) = after.split_once('}')?;
    Some(body)
}

/// The pair out of the environment. Absent is empty, which the crate reports.
fn from_env() -> (String, String) {
    let read = |key: &str| std::env::var(key).unwrap_or_default();
    (read(ID), read(SECRET))
}

/// One string field of a flat JSON object. Client ids and secrets carry no
/// quotes or escapes, so the value is whatever sits between the next pair of
/// quotes after the key.
fn field(text: &str, key: &str) -> Option<String> {
    let after = text.split_once(&format!("\"{key}\""))?.1;
    let after = after.split_once(':')?.1;
    let after = after.split_once('"')?.1;
    let (value, _) = after.split_once('"')?;
    Some(value.to_string())
}

/// What can go through `cargo:rustc-env`: one line, no surrounding space.
fn one_line(value: &str) -> String {
    value.replace(['\r', '\n'], "").trim().to_string()
}
