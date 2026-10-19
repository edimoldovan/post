//! Post's window — the third door onto the mail palette.
//!
//! There are already two: the assistant's `post_` tools and `post
//! <verb>` in a terminal. This is the one for reading, because nobody reads
//! email in a terminal. Four commands open it and each one *is* a view —
//! `post` the accounts, `post list <email>` that account's
//! inbox, `post read <email> <id>` that message, `post reply
//! <email> <id>` a reply to it — so the window needs no launcher and no menu
//! to be useful: the command is the navigation.
//!
//! It is only a view. The mail lives in [`post`], which is headless and stays
//! that way: nothing here is required for mail to work, closing the window
//! loses nothing, and every verb the window runs is a verb the assistant and
//! the terminal run too.
//!
//! - [`route`] — the routes and the router, including which command line
//!   means which view. Pure, and where the behaviour is pinned down.
//! - [`client`] — the only file that calls the mail library; hands up plain
//!   types and runs the network on threads of its own.
//! - [`state`] — `PostApp` and every way an answer changes what is on screen.
//! - [`app`] — the update loop and command dispatch.
//! - [`commands`] / [`keymap`] — the registry and the chords bound to it.
//! - [`ui`] — one `impl PostApp` block per area of the interface, including
//!   [`ui::compose`], the reply editor.
//! - [`html`] — the HTML body, drawn by Blitz.

pub mod app;
pub mod client;
pub mod commands;
pub mod keymap;
pub mod layout;
pub mod route;
pub mod state;
pub mod ui;
pub mod html;

pub use route::{Route, Router};
pub use state::PostApp;
