//! Where Post keeps things.
//!
//! Three levels, and only the middle one is a person's. Application config —
//! the Google OAuth client — is embedded at build time and never on this
//! computer. User config is small and hand-editable in `~/.config/post/`:
//! the connected accounts and one OAuth token file each. Anything rebuildable — fetched HTML
//! bodies — lives in the state dir instead, where deleting it costs nothing.
//! Attachments land in the user's download folder.

use std::path::{Path, PathBuf};

/// The user's home, or /tmp when there is no telling — the same fallback the
/// allowlist uses, so nothing here ever writes to the root of the filesystem.
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `$XDG_CONFIG_HOME/post`, or `~/.config/post`: the user config dir.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"))
        .join("post")
}

/// `$XDG_DATA_HOME/post`, or `~/.local/share/post`: the state dir.
pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".local").join("share"))
        .join("post")
}

/// Where an earlier version kept its config, one directory deeper. Config is
/// the person's, not the product's, so what is left down there moves up.
fn older_dir(config: &Path) -> PathBuf {
    config.join("post")
}

/// The connected Google accounts: `[{"email": "…"}]`.
pub fn accounts_file() -> PathBuf {
    accounts_file_in(&config_dir())
}

/// One OAuth token file per account, 0600 in a 0700 directory.
pub fn oauth_tokens_dir() -> PathBuf {
    oauth_tokens_dir_in(&config_dir())
}

/// The account list under `config`.
fn accounts_file_in(config: &Path) -> PathBuf {
    migrate_user_config(config);
    config.join("accounts.json")
}

/// The OAuth token directory under `config`.
fn oauth_tokens_dir_in(config: &Path) -> PathBuf {
    migrate_user_config(config);
    config.join("oauth-tokens")
}

/// Moves what an earlier version left one directory deeper up into `config`,
/// once each. Both moves run on the first path asked for, so a computer is
/// never left half-moved. The OAuth token directory had an earlier name still
/// — it was called `tokens` before it said what kind — and either one moves.
fn migrate_user_config(config: &Path) {
    let older = older_dir(config);
    if !older.is_dir() {
        return;
    }
    crate::store::migrate(&older.join("accounts.json"), &config.join("accounts.json"));
    let oauth_tokens = config.join("oauth-tokens");
    crate::store::migrate(&older.join("oauth-tokens"), &oauth_tokens);
    crate::store::migrate(&older.join("tokens"), &oauth_tokens);
    // Empty now, so it goes. Anything else left down there keeps it.
    let _ = std::fs::remove_dir(&older);
}

/// Fetched HTML bodies, one directory per account. Rebuildable: the provider
/// stays authoritative.
pub fn bodies_dir() -> PathBuf {
    state_dir().join("postbodies")
}

/// One fetched body. The address is a directory name here and the id a
/// filename, so nothing in either may be a path.
pub fn body_file(email: &str, id: &str) -> PathBuf {
    bodies_dir()
        .join(safe(email))
        .join(format!("{}.html", safe(id)))
}

/// The newest fetched page of each mailbox, one file per account. Rebuildable
/// like the bodies: it is a head start, never the truth.
pub fn list_cache_dir() -> PathBuf {
    state_dir().join("postlist")
}

/// One account's cached page. An address is a filename here, so nothing in it
/// may be a path.
pub fn list_cache_file(email: &str) -> PathBuf {
    list_cache_dir().join(format!("{}.json", safe(email)))
}

/// Messages already fetched, one directory per account. A message never
/// changes, so a message read once is read from here forever after.
pub fn read_cache_dir() -> PathBuf {
    state_dir().join("postread")
}

/// One message's cached read. Both the address and the id are filenames here,
/// so nothing in either may be a path.
pub fn read_cache_file(email: &str, id: &str) -> PathBuf {
    read_cache_dir()
        .join(safe(email))
        .join(format!("{}.json", safe(id)))
}

/// Everything kept on this computer for one account but its sign-in and its
/// entry: its cached page, the messages read, the bodies fetched.
pub fn kept_for(email: &str) -> Vec<PathBuf> {
    vec![
        list_cache_file(email),
        read_cache_dir().join(safe(email)),
        bodies_dir().join(safe(email)),
    ]
}

/// A name as a filename: nothing in it is allowed to be a path.
///
/// A separator is neutralised, and so is a name that is a directory rather
/// than a name — `.` and `..` are steps through the tree, not addresses — and
/// nothing ever comes back empty. An ordinary address is itself, byte for
/// byte, dots and all.
pub(crate) fn safe(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| if c == '/' || c == '\\' { '_' } else { c })
        .collect();
    if name.is_empty() {
        return "_".to_string();
    }
    // `.`, `..`, and anything else that is nothing but dots.
    if name.chars().all(|c| c == '.') {
        return "_".repeat(name.len());
    }
    name
}

/// Where a saved attachment lands: the user's download folder —
/// `XDG_DOWNLOAD_DIR` in `user-dirs.dirs`, or `~/Downloads` without one.
pub fn attachments_dir() -> PathBuf {
    let home = home();
    let dirs = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("user-dirs.dirs");
    std::fs::read_to_string(dirs)
        .ok()
        .and_then(|text| download_dir_in(&text, &home))
        .unwrap_or_else(|| home.join("Downloads"))
}

/// `XDG_DOWNLOAD_DIR` out of a `user-dirs.dirs`, with `$HOME` filled in.
fn download_dir_in(text: &str, home: &Path) -> Option<PathBuf> {
    let value = text
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("XDG_DOWNLOAD_DIR="))?
        .trim_matches('"');
    let path = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(value),
    };
    path.is_absolute().then_some(path)
}

/// Runs `work` with the state dir pointed at a scratch directory of this test
/// run's own, and hands it that directory.
///
/// The state dir comes out of the environment, and the environment belongs to
/// the process rather than to one test. So it is set once, always to the same
/// path, and every test that reads or writes under the state dir goes through
/// here: nothing reads the variable while it is being written.
#[cfg(test)]
pub(crate) fn with_state_dir<T>(work: impl FnOnce(&Path) -> T) -> T {
    use std::sync::{Mutex, OnceLock};

    static SCRATCH: OnceLock<Mutex<PathBuf>> = OnceLock::new();
    let scratch = SCRATCH.get_or_init(|| {
        Mutex::new(
            std::env::temp_dir()
                .join("post-state-tests")
                .join(std::process::id().to_string()),
        )
    });
    // A test that panicked left the lock poisoned, not the directory wrong.
    let home = scratch.lock().unwrap_or_else(|held| held.into_inner());
    // SAFETY: the only write of this variable in this process, under the lock
    // that every read of it is also taken under.
    unsafe { std::env::set_var("XDG_DATA_HOME", &*home) };
    work(&state_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{self, OauthTokens};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("post-paths-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch dir");
        dir
    }

    fn some_oauth_tokens() -> OauthTokens {
        OauthTokens {
            refresh_token: "1//refresh".to_string(),
            access_token: "ya29.access".to_string(),
            expiry: 1_757_000_000,
        }
    }

    /// A fresh computer: the paths are right under the config dir, and nothing
    /// is conjured on the way.
    #[test]
    fn with_nothing_to_move_the_paths_are_the_config_dir_itself() {
        let config = scratch("fresh");
        assert_eq!(accounts_file_in(&config), config.join("accounts.json"));
        assert_eq!(oauth_tokens_dir_in(&config), config.join("oauth-tokens"));
        assert!(!config.join("accounts.json").exists());
        assert!(!config.join("oauth-tokens").exists());
    }

    /// A computer that connected an account before config stopped being
    /// product-scoped keeps it: the list moves up once.
    #[test]
    fn an_older_account_list_is_moved_up_once() {
        let config = scratch("accounts-move");
        let older = older_dir(&config).join("accounts.json");
        store::add_account(&older, "ed@acme.com").expect("added");

        let file = accounts_file_in(&config);
        assert_eq!(file, config.join("accounts.json"));
        assert!(!older.exists(), "the older list is gone");
        assert_eq!(store::accounts(&file), vec!["ed@acme.com"]);

        // Once: a stray left by an older copy still running does not win.
        store::add_account(&older, "stray@acme.com").expect("added");
        let file = accounts_file_in(&config);
        assert!(older.exists(), "the stray stays where it is");
        assert_eq!(store::accounts(&file), vec!["ed@acme.com"]);
    }

    /// The same for the OAuth tokens — and the moved file is still nobody
    /// else's to read. Asking for the account list is enough to move them:
    /// whichever path is asked for first, the computer is never left half
    /// moved.
    #[test]
    fn an_older_oauth_token_directory_is_moved_up_once() {
        let config = scratch("oauth-tokens-move");
        let older = older_dir(&config).join("oauth-tokens");
        let tokens = some_oauth_tokens();
        store::save_oauth_tokens(&older, "ed@acme.com", &tokens).expect("saved");
        store::add_account(&older_dir(&config).join("accounts.json"), "ed@acme.com")
            .expect("added");

        // Only the account list is asked for.
        let file = accounts_file_in(&config);
        assert_eq!(store::accounts(&file), vec!["ed@acme.com"]);
        assert!(!older.exists(), "the older directory is gone");
        assert!(
            !older_dir(&config).exists(),
            "and so is the emptied directory it sat in"
        );

        let dir = oauth_tokens_dir_in(&config);
        assert_eq!(dir, config.join("oauth-tokens"));
        assert_eq!(
            store::load_oauth_tokens(&dir, "ed@acme.com").expect("loaded"),
            tokens
        );
        assert_eq!(
            store::mode(&store::oauth_token_file(&dir, "ed@acme.com")),
            Some(0o600)
        );
        assert_eq!(store::mode(&dir), Some(0o700));
    }

    /// An address is a filename in the caches, and a filename is not a path: a
    /// separator in one is neutralised, so the cache file for an address out of
    /// a model or a shell still lands in the directory it belongs to.
    #[test]
    fn an_address_is_a_filename_and_never_a_path() {
        with_state_dir(|state| {
            // One account's page is one file, named after the address.
            let page = list_cache_file("ed/../root@acme.com");
            assert_eq!(page.parent(), Some(state.join("postlist").as_path()));
            assert_eq!(
                page.file_name().and_then(|n| n.to_str()),
                Some("ed_.._root@acme.com.json")
            );

            // A Windows-shaped path is not a path here either.
            let back = list_cache_file(r"ed\..\root@acme.com");
            assert_eq!(back.parent(), Some(state.join("postlist").as_path()));
            assert_eq!(
                back.file_name().and_then(|n| n.to_str()),
                Some("ed_.._root@acme.com.json")
            );

            // A cached message is two names — the address and the id — and
            // both of them are filenames.
            assert_eq!(
                read_cache_file("ed/x@acme.com", "18f3a2c9b1"),
                state
                    .join("postread")
                    .join("ed_x@acme.com")
                    .join("18f3a2c9b1.json")
            );

            // A body is filed under the address too, and that is a directory
            // name: an absolute address does not take the base dir with it.
            assert_eq!(
                body_file("ed@acme.com", "18f3a2c9b1"),
                state
                    .join("postbodies")
                    .join("ed@acme.com")
                    .join("18f3a2c9b1.html")
            );
        });
    }

    /// An address out of a model or a shell is never a way out of the state
    /// dir: an absolute one does not discard the directory it is joined to,
    /// and `..` does not step up out of it.
    #[test]
    fn an_address_can_never_walk_out_of_the_state_dir() {
        with_state_dir(|state| {
            // Inside means two things: under the directory, and with no step
            // back out of it left in the path to walk.
            fn inside(file: &Path, dir: PathBuf, hostile: &str) {
                use std::path::Component;
                assert!(
                    file.starts_with(&dir),
                    "\"{hostile}\" landed at {}",
                    file.display()
                );
                assert!(
                    !file
                        .strip_prefix(&dir)
                        .expect("under the directory")
                        .components()
                        .any(|c| matches!(c, Component::ParentDir | Component::CurDir)),
                    "\"{hostile}\" kept a step back out: {}",
                    file.display()
                );
            }

            let escapes = ["/etc", "/etc/passwd", "..", "../..", "../../../etc", "."];
            for hostile in escapes {
                inside(&list_cache_file(hostile), state.join("postlist"), hostile);
                inside(
                    &read_cache_file(hostile, "18f3a2c9b1"),
                    state.join("postread"),
                    hostile,
                );
                inside(
                    &body_file(hostile, "18f3a2c9b1"),
                    state.join("postbodies"),
                    hostile,
                );
            }

            // Named exactly: `/etc` is a name, and `..` is two underscores.
            assert_eq!(
                body_file("/etc", "18f3a2c9b1"),
                state
                    .join("postbodies")
                    .join("_etc")
                    .join("18f3a2c9b1.html")
            );
            assert_eq!(
                read_cache_file("..", "18f3a2c9b1"),
                state.join("postread").join("__").join("18f3a2c9b1.json")
            );
        });
    }

    /// What a filename is allowed to carry: everything but a separator.
    #[test]
    fn a_separator_is_the_one_thing_a_name_may_not_keep() {
        for hostile in ["a/b", r"a\b", "//", r"\\", "/etc/passwd", "ed@acme.com"] {
            let name = safe(hostile);
            assert!(!name.contains('/'), "{hostile} kept a slash: {name}");
            assert!(!name.contains('\\'), "{hostile} kept a backslash: {name}");
            assert_eq!(name.chars().count(), hostile.chars().count());
        }
        assert_eq!(safe("ed@acme.com"), "ed@acme.com", "an ordinary address is itself");
    }

    /// The other thing a name may not be: a directory. `.` and `..` are steps
    /// through the tree rather than names, so neither survives as one — and
    /// nothing ever comes back empty, which is no filename at all.
    #[test]
    fn a_dot_directory_is_not_a_name_either() {
        use std::path::Component;

        for hostile in ["", ".", "..", "...", "/..", "../..", r"..\..", "/"] {
            let name = safe(hostile);
            assert!(!name.is_empty(), "\"{hostile}\" became nothing");
            assert!(
                !Path::new(&name)
                    .components()
                    .any(|c| matches!(c, Component::ParentDir | Component::CurDir)),
                "\"{hostile}\" kept a step through the tree: {name}"
            );
        }
        assert_eq!(safe("."), "_");
        assert_eq!(safe(".."), "__");

        // And the dots an ordinary address is full of are kept, every one.
        assert_eq!(safe("ed.moldovan@acme.co.uk"), "ed.moldovan@acme.co.uk");
        assert_eq!(safe(".hidden@acme.com"), ".hidden@acme.com");
    }

    /// The oldest shape of all: a directory named before it said what kind of
    /// token it held. It moves up too.
    #[test]
    fn a_directory_from_before_the_oauth_rename_is_moved_up_too() {
        let config = scratch("oauth-tokens-oldest");
        let oldest = older_dir(&config).join("tokens");
        let tokens = some_oauth_tokens();
        store::save_oauth_tokens(&oldest, "ed@acme.com", &tokens).expect("saved");

        let dir = oauth_tokens_dir_in(&config);
        assert!(!oldest.exists(), "the oldest directory is gone");
        assert_eq!(
            store::load_oauth_tokens(&dir, "ed@acme.com").expect("loaded"),
            tokens
        );
        assert_eq!(store::mode(&dir), Some(0o700));
    }
}
