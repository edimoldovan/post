//! Post — the company's mail, as a library.
//!
//! Post is a bridge, never a mail service: the company keeps Gmail and its
//! addresses, and this crate speaks the Gmail API on its behalf. V1 is Google
//! only.
//!
//! There is no daemon and no server here. The account list and the OAuth tokens
//! are files under `~/.config/post/`, so anything that needs mail reads
//! them: the MCP shim answers `post_*` in its own process, `post <verb>`
//! runs the same functions from a terminal, and the window (later) will be a
//! view of the same files. Nothing owns mail; whoever asks, reads.
//!
//! - [`paths`] — where everything lives on disk.
//! - [`store`] — the account list, the OAuth tokens (0600) and the OAuth
//!   client, which is embedded at build time.
//! - [`oauth`] — code+PKCE through the system browser, on a loopback port.
//! - [`gmail`] — the REST calls: list, read, mark, trash, attachment, send.
//! - [`mime`] — the pure parts: base64url, MIME walking, HTML to text, dates.
//! - [`reply`] — answering a message: the draft, and sending it.
//! - [`verbs`] — the palette: one table of verbs that both doors dispatch off.
//! - [`claude`] — letting Claude use Post: its MCP entry and its skill file.

pub mod claude;
pub mod gmail;
pub mod mime;
pub mod oauth;
pub mod paths;
pub mod reply;
pub mod store;
pub mod verbs;

/// What every mail verb is called, in MCP and on the command line alike.
pub const PREFIX: &str = "post_";
