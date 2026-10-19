use eframe::egui;
use crate::role::{self, Role};
use crate::{mcp, posthost};
use post_ui::PostApp;

/// Runs the role a command line asks for.
pub fn run(args: &[String]) -> eframe::Result {
    match role::of(args) {
        // The stdio shim an MCP client launches. It answers the handshake and
        // the read-only tools itself and opens nothing; anything that acts goes
        // to the server, which is the only place the user can be asked first.
        Role::Shim => {
            if let Err(e) = mcp::proto::stdio_shim() {
                eprintln!("post mcp: {e}");
                std::process::exit(1);
            }
            Ok(())
        }

        // `post <verb>`: the terminal door onto the mail palette. The same
        // library calls the assistant's post_ tools make.
        Role::Post { words } => {
            match post::verbs::cli(&words) {
                Ok(text) => {
                    if !text.is_empty() {
                        println!("{text}");
                    }
                }
                Err(e) => {
                    eprintln!("post: {e}");
                    std::process::exit(1);
                }
            }
            Ok(())
        }

        // The mail verbs that are views: `post`, `post list <email>`,
        // `post read <email> <id>`. Reading is what a window is for, so these
        // put one on screen instead of printing — or hand the view to the one
        // already up. Either way the prompt comes straight back: the window is
        // a detached process of its own, never this one.
        Role::PostOpen { route } => {
            if let Err(e) = posthost::open(&route) {
                eprintln!("post: {e}");
                std::process::exit(1);
            }
            Ok(())
        }

        // And that detached process, reading its own role back.
        Role::PostWindow { route } => post_window(route),

        // The server: the socket and the confirmation broker. No window of its
        // own, so it needs no display to run.
        Role::Serve => {
            crate::serve::run();
            Ok(())
        }

        // One confirmation dialog, for the question on stdin.
        Role::Confirm => crate::dialog::run(),
    }
}

/// Shows Post at one view — or finds that a Post window already is, and hands
/// the view over instead of opening a second one.
fn post_window(route: post_ui::Route) -> eframe::Result {
    let open_rx = match posthost::start(&route) {
        posthost::Outcome::HandedOff => return Ok(()),
        posthost::Outcome::Run(rx) => rx,
    };

    // When the user let Claude use Post, keep that registration pointing at
    // this binary.
    std::thread::spawn(post::claude::refresh);

    let options = eframe::NativeOptions {
        // A Wayland window that is not on screen gets no frame callbacks, and
        // a vsynced swap waits for one: the loop would stop answering the
        // compositor, which then calls Post not responding. egui only paints
        // when something changed, so nothing is lost without it.
        vsync: false,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_title("Post")
            // Wayland's half of the same name `posthost` spawns the window
            // under; X11 takes it from argv[0] instead.
            .with_app_id(posthost::APP_ID),
        ..Default::default()
    };

    let outcome = eframe::run_native(
        posthost::APP_ID,
        options,
        Box::new(move |cc| {
            omarchy_kit::fonts::install(&cc.egui_ctx);
            cc.egui_ctx
                .set_visuals(omarchy_kit::theme::visuals(&omarchy_kit::theme::load()));
            posthost::set_wake(cc.egui_ctx.clone());
            let mut app = PostApp::new(route);
            app.wake = Some(cc.egui_ctx.clone());
            app.open_rx = Some(open_rx);
            Ok(Box::new(app))
        }),
    );
    // The window is gone: so is the door, or the next launch would talk to it.
    posthost::release();
    outcome
}
