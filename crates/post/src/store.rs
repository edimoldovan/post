//! The account list and the OAuth tokens — both files. The OAuth client is not:
//! it is embedded at build time, because it belongs to the application rather
//! than to whoever runs it.
//!
//! Every function here takes the file or directory it works on, so the callers
//! pass [`crate::paths`] and the tests pass a scratch directory. An OAuth token
//! file is written 0600 and its directory 0700: a refresh token is as good as
//! the mailbox it opens.

use std::fs::{File, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// One connected mailbox: its address, and the name it goes by where the
/// user gave it one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The OAuth tokens Google gave us for one account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OauthTokens {
    /// The long-lived one: it is what makes the account stay connected.
    pub refresh_token: String,
    pub access_token: String,
    /// Unix seconds after which the access token is no longer worth sending.
    pub expiry: u64,
}

/// The OAuth client this build signs in with — a Desktop client from the
/// Google Cloud console, embedded by `build.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Client {
    pub client_id: String,
    pub client_secret: String,
}

/// Seconds since the epoch.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The connected accounts. A missing file is no accounts, which is the state
/// every computer starts in.
pub fn accounts(file: &Path) -> Vec<String> {
    account_list(file).into_iter().map(|a| a.email).collect()
}

/// The connected accounts, names and all.
pub fn account_list(file: &Path) -> Vec<Account> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<Account>>(&text).unwrap_or_default()
}

/// Adds an account, or leaves the list alone if it is already there.
pub fn add_account(file: &Path, email: &str) -> Result<(), String> {
    let mut list = account_list(file);
    if !list.iter().any(|a| a.email == email) {
        list.push(Account {
            email: email.to_string(),
            name: None,
        });
    }
    write_accounts(file, &list)
}

/// Gives an account the name it goes by. An empty name is no name: the
/// account goes by its address again.
pub fn rename_account(file: &Path, email: &str, name: &str) -> Result<(), String> {
    let mut list = account_list(file);
    let account = list
        .iter_mut()
        .find(|a| a.email == email)
        .ok_or_else(|| format!("{email} is not connected"))?;
    let name = name.trim();
    account.name = (!name.is_empty() && name != email).then(|| name.to_string());
    write_accounts(file, &list)
}

/// Takes an account off the list.
pub fn remove_account(file: &Path, email: &str) -> Result<(), String> {
    let mut list = account_list(file);
    list.retain(|a| a.email != email);
    write_accounts(file, &list)
}

fn write_accounts(file: &Path, accounts: &[Account]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(accounts)
        .map_err(|e| format!("could not write the account list: {e}"))?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    std::fs::write(file, format!("{text}\n"))
        .map_err(|e| format!("could not write {}: {e}", file.display()))
}

/// Where one account's OAuth tokens live.
pub fn oauth_token_file(dir: &Path, email: &str) -> PathBuf {
    // An address is a filename here, so nothing in it may be a path — the one
    // rule for that lives in `paths`.
    dir.join(format!("{}.json", crate::paths::safe(email)))
}

/// Reads one account's OAuth tokens.
pub fn load_oauth_tokens(dir: &Path, email: &str) -> Result<OauthTokens, String> {
    let file = oauth_token_file(dir, email);
    let text = std::fs::read_to_string(&file).map_err(|_| {
        format!("{email} is not connected on this computer — run: post add-account")
    })?;
    serde_json::from_str(&text).map_err(|e| format!("{} is not readable: {e}", file.display()))
}

/// Writes one account's OAuth tokens, 0600, in a 0700 directory.
pub fn save_oauth_tokens(dir: &Path, email: &str, tokens: &OauthTokens) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let _ = std::fs::set_permissions(dir, Permissions::from_mode(0o700));

    let file = oauth_token_file(dir, email);
    let text = serde_json::to_string_pretty(tokens)
        .map_err(|e| format!("could not write the OAuth tokens: {e}"))?;
    // The mode is set on creation, and again after, so a file left behind by an
    // earlier version cannot stay readable.
    let mut out = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&file)
        .map_err(|e| format!("could not write {}: {e}", file.display()))?;
    out.write_all(format!("{text}\n").as_bytes())
        .map_err(|e| format!("could not write {}: {e}", file.display()))?;
    let _ = std::fs::set_permissions(&file, Permissions::from_mode(0o600));
    Ok(())
}

/// Forgets one account's OAuth tokens. Used when a refresh is refused for good.
pub fn forget_oauth_tokens(dir: &Path, email: &str) {
    let _ = std::fs::remove_file(oauth_token_file(dir, email));
}

/// Moves what an earlier version left at `older` — a file or a whole directory
/// — to where this one looks, once. Nothing to move, or something there
/// already, is nothing to do. A rename, so an OAuth token file keeps its 0600
/// and its directory its 0700.
pub fn migrate(older: &Path, newer: &Path) {
    if newer.exists() || !older.exists() {
        return;
    }
    if let Some(dir) = newer.parent()
        && std::fs::create_dir_all(dir).is_err()
    {
        return;
    }
    let _ = std::fs::rename(older, newer);
}

/// What to say when this build has no Google client in it. The fix is one file,
/// so it is one sentence.
const NO_CLIENT: &str = "this build has no Google client — put \
                         {\"google\":{\"client_id\":…,\"client_secret\":…}} in \
                         config/oauth.json (or set \
                         POST_GOOGLE_CLIENT_ID/SECRET) and rebuild";

/// The OAuth client embedded at build time, or the sentence that says what to
/// put where. Nothing on this computer decides it: one client belongs to the
/// application, and `build.rs` baked it in.
pub fn client() -> Result<Client, String> {
    embedded(
        env!("POST_GOOGLE_CLIENT_ID"),
        env!("POST_GOOGLE_CLIENT_SECRET"),
    )
}

/// The embedded pair as a client. Either half missing is no client at all.
fn embedded(client_id: &str, client_secret: &str) -> Result<Client, String> {
    if client_id.trim().is_empty() || client_secret.trim().is_empty() {
        return Err(NO_CLIENT.to_string());
    }
    Ok(Client {
        client_id: client_id.trim().to_string(),
        client_secret: client_secret.trim().to_string(),
    })
}

/// A file's permission bits, for tests and for anything that wants to check.
pub fn mode(file: &Path) -> Option<u32> {
    std::fs::metadata(file)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
}

/// Creates `file`'s parent, then the file itself, and hands it over for
/// writing. Used for fetched HTML bodies.
pub fn create(file: &Path) -> Result<File, String> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    File::create(file).map_err(|e| format!("could not write {}: {e}", file.display()))
}

#[cfg(test)]
mod tests {

    /// An account goes by its address until it is given a name; an empty name
    /// puts the address back; removing it takes only it off the list — and a
    /// list written before names existed reads as it always did.
    #[test]
    fn an_account_can_be_named_and_removed() {
        let dir = std::env::temp_dir()
            .join("post-store-tests")
            .join(format!("names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("accounts.json");
        std::fs::create_dir_all(&dir).expect("a dir");
        std::fs::write(&file, r#"[{"email": "ed@acme.com"}]"#).expect("an old list");
        assert_eq!(accounts(&file), vec!["ed@acme.com".to_string()]);

        add_account(&file, "ale@acme.com").expect("added");
        rename_account(&file, "ed@acme.com", "Eduard Moldovan AB").expect("renamed");
        let list = account_list(&file);
        assert_eq!(list[0].name.as_deref(), Some("Eduard Moldovan AB"));
        assert_eq!(list[1].name, None, "the other is its address");
        add_account(&file, "ed@acme.com").expect("connected again");
        assert_eq!(account_list(&file)[0].name.as_deref(), Some("Eduard Moldovan AB"), "kept");

        rename_account(&file, "ed@acme.com", "  ").expect("renamed back");
        assert_eq!(account_list(&file)[0].name, None);
        assert!(rename_account(&file, "nobody@acme.com", "x").is_err());

        remove_account(&file, "ed@acme.com").expect("removed");
        assert_eq!(accounts(&file), vec!["ale@acme.com".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("post-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch dir");
        dir
    }

    #[test]
    fn an_account_list_that_is_not_there_yet_is_empty() {
        let dir = scratch("no-accounts");
        assert!(accounts(&dir.join("accounts.json")).is_empty());
    }

    #[test]
    fn accounts_round_trip_and_do_not_double_up() {
        let dir = scratch("accounts");
        // Nested: the first account also has to make the config dir.
        let file = dir.join("post").join("accounts.json");
        add_account(&file, "ed@acme.com").expect("added");
        add_account(&file, "ale@acme.com").expect("added");
        add_account(&file, "ed@acme.com").expect("added again");
        assert_eq!(accounts(&file), vec!["ed@acme.com", "ale@acme.com"]);
        // Hand-editable, and in the shape the plan says.
        let text = std::fs::read_to_string(&file).expect("read");
        assert!(text.contains("\"email\": \"ed@acme.com\""), "got {text}");
    }

    #[test]
    fn oauth_tokens_round_trip_and_stay_private() {
        let dir = scratch("oauth-tokens");
        let tokens = OauthTokens {
            refresh_token: "1//refresh".to_string(),
            access_token: "ya29.access".to_string(),
            expiry: 1_757_000_000,
        };
        save_oauth_tokens(&dir, "ed@acme.com", &tokens).expect("saved");
        assert_eq!(
            load_oauth_tokens(&dir, "ed@acme.com").expect("loaded"),
            tokens
        );
        assert_eq!(mode(&oauth_token_file(&dir, "ed@acme.com")), Some(0o600));
        assert_eq!(mode(&dir), Some(0o700));

        // Written twice, still 0600 — and still the last value.
        let newer = OauthTokens {
            access_token: "ya29.newer".to_string(),
            ..tokens.clone()
        };
        save_oauth_tokens(&dir, "ed@acme.com", &newer).expect("saved again");
        assert_eq!(
            load_oauth_tokens(&dir, "ed@acme.com").expect("loaded"),
            newer
        );
        assert_eq!(mode(&oauth_token_file(&dir, "ed@acme.com")), Some(0o600));
    }

    #[test]
    fn an_account_that_was_never_connected_says_so() {
        let dir = scratch("no-oauth-tokens");
        let e = load_oauth_tokens(&dir, "nobody@acme.com").expect_err("no OAuth tokens");
        assert!(e.contains("not connected"), "got {e}");
        assert!(e.contains("add-account"), "got {e}");
    }

    /// A computer set up by an earlier version must not have to be set up
    /// again: whatever it left behind moves over, once, with its modes intact.
    #[test]
    fn what_an_earlier_version_left_behind_is_moved_over_once() {
        let older = scratch("migrate-from");
        let newer = scratch("migrate-to").join("moved");

        // A whole directory, and the OAuth token file in it keeps its 0600.
        let tokens = OauthTokens {
            refresh_token: "1//refresh".to_string(),
            access_token: "ya29.access".to_string(),
            expiry: 1_757_000_000,
        };
        save_oauth_tokens(&older, "ed@acme.com", &tokens).expect("saved");
        migrate(&older, &newer);
        assert!(!older.exists(), "the older directory is gone");
        assert_eq!(
            load_oauth_tokens(&newer, "ed@acme.com").expect("loaded"),
            tokens
        );
        assert_eq!(mode(&oauth_token_file(&newer, "ed@acme.com")), Some(0o600));
        assert_eq!(mode(&newer), Some(0o700));

        // Once: a stray left by an older copy still running does not overwrite
        // what is already connected.
        let stale = OauthTokens {
            refresh_token: "1//stale".to_string(),
            ..tokens.clone()
        };
        save_oauth_tokens(&older, "ed@acme.com", &stale).expect("saved");
        migrate(&older, &newer);
        assert!(older.is_dir(), "the stray stays where it is");
        assert_eq!(
            load_oauth_tokens(&newer, "ed@acme.com").expect("loaded"),
            tokens
        );
    }

    /// A single file moves the same way, and the directory it lands in is made.
    #[test]
    fn a_lone_file_is_moved_into_a_directory_that_is_made_for_it() {
        let dir = scratch("migrate-file");
        let older = dir.join("older").join("accounts.json");
        let newer = dir.join("newer").join("accounts.json");
        add_account(&older, "ed@acme.com").expect("added");

        migrate(&older, &newer);
        assert!(!older.exists(), "the older file is gone");
        assert_eq!(accounts(&newer), vec!["ed@acme.com"]);
    }

    /// Nothing to migrate is nothing to do — the every-other-time case.
    #[test]
    fn nothing_to_migrate_is_nothing_to_do() {
        let dir = scratch("migrate-nothing");
        let newer = dir.join("oauth-tokens");
        migrate(&dir.join("gone"), &newer);
        assert!(!newer.exists(), "nothing is conjured");
    }

    /// Built without a client, every op that needs one says what to put where.
    #[test]
    fn a_build_with_no_google_client_says_what_to_put_where() {
        let e = embedded("", "").expect_err("no client");
        assert!(!e.contains('\n'), "one sentence: got {e}");
        assert!(
            e.contains(
                "this build has no Google client — put \
                 {\"google\":{\"client_id\":…,\"client_secret\":…}} in \
                 config/oauth.json"
            ),
            "got {e}"
        );
        assert!(e.contains("POST_GOOGLE_CLIENT_ID/SECRET) and rebuild"), "got {e}");
        // Half a client is no client.
        assert!(embedded("123.apps.googleusercontent.com", "  ").is_err());
        assert!(embedded(" ", "GOCSPX-secret").is_err());
    }

    /// The embedded pair, whichever way this copy was built: a whole client, or
    /// the sentence that says how to put one in.
    #[test]
    fn the_embedded_client_is_whole_or_it_says_so() {
        match client() {
            Ok(client) => {
                assert!(!client.client_id.is_empty());
                assert!(!client.client_secret.is_empty());
            }
            Err(e) => assert!(e.contains("this build has no Google client"), "got {e}"),
        }
        // Surrounding space is the build's, not the value's.
        let trimmed = embedded(" 123.apps.googleusercontent.com\t", " GOCSPX-secret ")
            .expect("a client");
        assert_eq!(trimmed.client_id, "123.apps.googleusercontent.com");
        assert_eq!(trimmed.client_secret, "GOCSPX-secret");
    }
}
