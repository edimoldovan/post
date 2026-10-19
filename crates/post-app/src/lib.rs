//! Post: the user's Gmail, as a window, a terminal command and an assistant's
//! tools.
//!
//! The binary is a thin host around its roles, all the same executable:
//!
//! - `post mcp` — the stdio shim an assistant launches. Windowless.
//! - `post <verb>` — one mail verb from a terminal, off the same palette the
//!   assistant's `post_` tools use. Windowless too — except for the verbs
//!   that are views rather than answers (`post`, `post list`, `post read`),
//!   which open the window at that view.
//! - `post --serve` — the server: it owns the socket and the confirmation
//!   broker. One per user, and headless.
//! - `post --confirm` — one confirmation dialog, started by the server for
//!   one question and gone once it is answered.
//! - `post --window <view>` — the window, started detached by the roles that
//!   open one.

pub mod app;
pub mod confirm;
pub mod dialog;
pub mod ipc;
pub mod mcp;
pub mod posthost;
pub mod role;
pub mod serve;
