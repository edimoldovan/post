//! `post`: the window, the mail verbs in a terminal, and the MCP server an
//! assistant reaches Post through. The arguments decide which.

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    post_app::app::run(&args)
}
