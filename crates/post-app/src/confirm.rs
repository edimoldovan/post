//! The confirmation broker: nothing acts on the user's behalf unasked.
//!
//! A tool that only looks at something runs straight away. A tool that *does*
//! something — sends a mail, deletes a draft, takes an account off this computer — stops
//! here first. The call is parked, a dialog shows one sentence of what is about
//! to happen, and the answer comes from a click in that dialog and nowhere
//! else: there is no approve message on any socket, so nothing an assistant can
//! say counts as consent.
//!
//! The server has no window of its own, so the dialog is its own short-lived
//! process — `post --confirm`, one per question, started by
//! [`serve_dialogs`]. It gets the question on its stdin and answers on its
//! stdout, and that pipe is the only channel an approval travels on. A child
//! that crashes, hangs or says nothing is a refusal.
//!
//! No answer within [`TIMEOUT`] is a refusal — counted down in the dialog, and
//! again here as a fuse, because the two are not the same process. "Always
//! allow this tool" is the one way the question stops being asked, and it is
//! written down in `~/.config/post/allowlist.toml` where it can be read and
//! deleted.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// How long a question waits before it counts as a refusal.
pub const TIMEOUT: Duration = Duration::from_secs(120);

/// How often the asking thread looks again with nothing to ask: short enough
/// that a fuse burning out behind the current dialog is noticed.
const POLL: Duration = Duration::from_millis(250);

/// What the model is told when the answer is no. Same words either way: a
/// refusal is a refusal whether it was clicked or waited out.
pub const DENIED: &str = "denied by the user";

/// Tools that may never be allowed for good.
///
/// "Always allow this tool" is a standing yes, and there are two things no
/// standing yes covers: mail leaving in the user's name, and an account taken
/// off this computer. Each is its own question, and the dialog does not even
/// offer the checkbox.
pub fn always_asks(tool: &str) -> bool {
    tool == post::verbs::SEND || tool == post::verbs::REMOVE
}

/// Tools that do something rather than look at something.
///
/// Each product owns the answer for its own verbs; an unknown name under a
/// known prefix counts as acting, so a tool added later is asked about until
/// someone says otherwise. Every mail verb but three reads, flips a read flag,
/// moves a message to a trash it can be pulled back out of, writes a draft
/// without sending it, or names an account; `post_send`, `post_draft_delete`,
/// which deletes a draft for good, and `post_remove`, which takes an account
/// off this computer, are the ones that stop here.
pub fn acting(tool: &str) -> bool {
    if tool.starts_with(post::PREFIX) {
        // post_show is not on the palette and never will be: it reads nothing
        // and writes nothing, it only puts the window on screen at a view. Free
        // like the verbs that look.
        if tool == crate::mcp::tools::POST_SHOW {
            return false;
        }
        return post::verbs::acts(tool);
    }
    false
}

/// The one sentence the confirm dialog shows.
pub fn summary(tool: &str, args: &Value) -> String {
    if let Some((sentence, _)) = post::verbs::confirmation(tool, args) {
        return sentence;
    }
    format!("Run {tool}.")
}

/// What the dialog shows under the sentence.
///
/// Usually the arguments, shortened to something a person can glance at. A
/// reply is the exception: what is shown is the message itself, whole — a body
/// cut off at 160 characters is not something anyone can say yes to.
pub fn digest(tool: &str, args: &Value) -> String {
    if let Some((_, message)) = post::verbs::confirmation(tool, args) {
        return message;
    }
    let text = match args {
        Value::Object(map) if map.is_empty() => String::new(),
        other => other.to_string(),
    };
    if text.chars().count() > 160 {
        let short: String = text.chars().take(157).collect();
        format!("{short}…")
    } else {
        text
    }
}

/// The answer to one question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Deny,
}

/// A question waiting for an answer, as the confirm window sees it.
#[derive(Debug, Clone)]
pub struct Waiting {
    pub id: u64,
    pub tool: String,
    pub summary: String,
    pub digest: String,
    /// Seconds left before this counts as a refusal.
    pub left: u64,
}

struct Pending {
    id: u64,
    tool: String,
    summary: String,
    digest: String,
    deadline: Instant,
    answer: Sender<Decision>,
}

struct State {
    next_id: u64,
    queue: Vec<Pending>,
    allowed: BTreeSet<String>,
    file: PathBuf,
}

/// The broker itself. One per server process; shared by every socket thread.
pub struct Broker {
    state: Mutex<State>,
    timeout: Duration,
    /// Signalled when a question is queued, so the thread that asks them wakes
    /// without polling for it.
    asked: Condvar,
}

impl Broker {
    /// Loads the allowlist from `file` (missing is fine) and waits for work.
    pub fn new(file: PathBuf) -> Broker {
        Broker::with_timeout(file, TIMEOUT)
    }

    /// The same with a shorter fuse, which is how the tests watch one burn out.
    pub fn with_timeout(file: PathBuf, timeout: Duration) -> Broker {
        Broker {
            state: Mutex::new(State {
                next_id: 1,
                queue: Vec::new(),
                allowed: load(&file),
                file,
            }),
            timeout,
            asked: Condvar::new(),
        }
    }

    /// True if this tool may act without being asked about.
    pub fn allowed(&self, tool: &str) -> bool {
        self.state
            .lock()
            .map(|s| s.allowed.contains(tool))
            .unwrap_or(false)
    }

    /// The gate every tool call passes through.
    ///
    /// Returns immediately for a tool that only reads, and for one the user has
    /// already allowed for good. Otherwise it blocks until the dialog is
    /// answered, or until the fuse burns out — which is a refusal.
    pub fn decide(&self, tool: &str, args: &Value) -> Result<(), String> {
        if !acting(tool) {
            return Ok(());
        }
        let (answer, wait) = channel();
        let id = {
            let mut state = self.state.lock().map_err(|_| DENIED.to_string())?;
            if state.allowed.contains(tool) && !always_asks(tool) {
                return Ok(());
            }
            let id = state.next_id;
            state.next_id += 1;
            state.queue.push(Pending {
                id,
                tool: tool.to_string(),
                summary: summary(tool, args),
                digest: digest(tool, args),
                deadline: Instant::now() + self.timeout,
                answer,
            });
            id
        };
        self.nudge();

        match wait.recv_timeout(self.timeout + Duration::from_secs(1)) {
            Ok(Decision::Approve) => Ok(()),
            Ok(Decision::Deny) => Err(DENIED.to_string()),
            Err(_) => {
                // Nobody was there. Drop the question so the window stops
                // showing it, and refuse.
                self.drop_pending(id);
                Err(format!(
                    "{DENIED} — no answer within {} s",
                    self.timeout.as_secs()
                ))
            }
        }
    }

    /// The oldest unanswered question, with its countdown. `None` when there is
    /// nothing to ask, which is when no dialog is on screen.
    pub fn front(&self) -> Option<Waiting> {
        let Ok(state) = self.state.lock() else {
            return None;
        };
        let now = Instant::now();
        state.queue.first().map(|p| Waiting {
            id: p.id,
            tool: p.tool.clone(),
            summary: p.summary.clone(),
            digest: p.digest.clone(),
            left: p.deadline.saturating_duration_since(now).as_secs(),
        })
    }

    /// How many questions are waiting.
    pub fn waiting(&self) -> usize {
        self.state.lock().map(|s| s.queue.len()).unwrap_or(0)
    }

    /// Blocks until there is a question to ask, refusing on the way anything
    /// whose fuse burned out while it queued behind another dialog.
    pub fn next_question(&self) -> Waiting {
        loop {
            self.expire();
            if let Some(question) = self.front() {
                return question;
            }
            match self.state.lock() {
                Ok(state) => {
                    let _ = self.asked.wait_timeout(state, POLL);
                }
                Err(_) => std::thread::sleep(POLL),
            }
        }
    }

    /// Answers one question. `always` on an approval writes the tool into the
    /// allowlist, and lets through anything else already queued for it.
    pub fn resolve(&self, id: u64, decision: Decision, always: bool) {
        let mut send_now: Vec<(Sender<Decision>, Decision)> = Vec::new();
        if let Ok(mut state) = self.state.lock() {
            let Some(at) = state.queue.iter().position(|p| p.id == id) else {
                return;
            };
            let pending = state.queue.remove(at);
            if always && decision == Decision::Approve && !always_asks(&pending.tool) {
                state.allowed.insert(pending.tool.clone());
                let file = state.file.clone();
                let allowed = state.allowed.clone();
                if let Err(e) = save(&file, &allowed) {
                    eprintln!("post: could not write {}: {e}", file.display());
                }
                // Same tool, same answer, for anything already in the queue.
                let tool = pending.tool.clone();
                let mut i = 0;
                while i < state.queue.len() {
                    if state.queue[i].tool == tool {
                        let other = state.queue.remove(i);
                        send_now.push((other.answer, Decision::Approve));
                    } else {
                        i += 1;
                    }
                }
            }
            send_now.push((pending.answer, decision));
        }
        for (answer, decision) in send_now {
            let _ = answer.send(decision);
        }
    }

    /// Refuses anything whose countdown has run out. The waiting thread gives
    /// up on its own too; this is the fuse, and it burns whether or not a
    /// dialog ever came up.
    pub fn expire(&self) {
        let mut expired: Vec<Sender<Decision>> = Vec::new();
        if let Ok(mut state) = self.state.lock() {
            let now = Instant::now();
            let mut i = 0;
            while i < state.queue.len() {
                if state.queue[i].deadline <= now {
                    expired.push(state.queue.remove(i).answer);
                } else {
                    i += 1;
                }
            }
        }
        for answer in expired {
            let _ = answer.send(Decision::Deny);
        }
    }

    fn drop_pending(&self, id: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.queue.retain(|p| p.id != id);
        }
    }

    fn nudge(&self) {
        self.asked.notify_all();
    }
}

/// One question as the dialog process receives it: everything it has to draw,
/// and nothing else. It knows no ids and holds no handle on the broker, so the
/// only thing it can do is answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub tool: String,
    /// The one sentence: what is about to happen.
    pub sentence: String,
    /// What is shown under the sentence: the arguments, shortened — or, for a
    /// reply, the whole message that would leave.
    pub args: String,
    /// What the "always allow" checkbox says, and empty for a tool that may
    /// never be allowed for good — the dialog then has no checkbox at all.
    pub always_label: String,
    /// Seconds on the countdown before the dialog refuses by itself.
    pub seconds: u64,
}

impl Request {
    /// The question at the front of the queue, as the dialog will see it.
    pub fn about(question: &Waiting) -> Request {
        Request {
            tool: question.tool.clone(),
            sentence: question.summary.clone(),
            args: question.digest.clone(),
            always_label: if always_asks(&question.tool) {
                String::new()
            } else {
                format!("Always allow {}", question.tool)
            },
            seconds: question.left,
        }
    }

    /// The single line this travels as, on the child's stdin.
    pub fn encode(&self) -> String {
        json!({
            "tool": self.tool,
            "sentence": self.sentence,
            "args": self.args,
            "always_label": self.always_label,
            "seconds": self.seconds,
        })
        .to_string()
    }

    pub fn parse(line: &str) -> Option<Request> {
        let value: Value = serde_json::from_str(line.trim()).ok()?;
        let text = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let tool = value.get("tool")?.as_str()?.to_string();
        let always_label = match value.get("always_label").and_then(Value::as_str) {
            Some(label) => label.to_string(),
            None => format!("Always allow {tool}"),
        };
        Some(Request {
            tool,
            sentence: text("sentence"),
            args: text("args"),
            always_label,
            seconds: value
                .get("seconds")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| TIMEOUT.as_secs()),
        })
    }
}

/// What the dialog says back, on its stdout. Anything else — a crash, silence,
/// a line that will not parse — is [`Answer::deny`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    pub decision: Decision,
    pub always_allow: bool,
}

impl Answer {
    pub fn deny() -> Answer {
        Answer {
            decision: Decision::Deny,
            always_allow: false,
        }
    }

    pub fn encode(&self) -> String {
        json!({
            "decision": match self.decision {
                Decision::Approve => "approve",
                Decision::Deny => "deny",
            },
            "always_allow": self.always_allow,
        })
        .to_string()
    }

    pub fn parse(line: &str) -> Option<Answer> {
        let value: Value = serde_json::from_str(line.trim()).ok()?;
        let decision = match value.get("decision")?.as_str()? {
            "approve" => Decision::Approve,
            "deny" => Decision::Deny,
            _ => return None,
        };
        Some(Answer {
            decision,
            always_allow: value
                .get("always_allow")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// Asks the queue's questions, one dialog at a time, for as long as the server
/// runs. The next child starts when the last one has answered, so there is
/// never a second dialog on screen competing for the same click.
pub fn serve_dialogs(broker: &Broker) {
    loop {
        let question = broker.next_question();
        let answer = ask(&Request::about(&question));
        broker.resolve(question.id, answer.decision, answer.always_allow);
    }
}

/// Runs one dialog process and waits for its one line.
///
/// The child is killed on the way out whatever it said, so a dialog never
/// outlives the question it was asked, and nothing is left running.
pub fn ask(request: &Request) -> Answer {
    let mut child = match dialog_command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("post: could not ask the user ({e}) — refusing");
            return Answer::deny();
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{}", request.encode());
        let _ = stdin.flush();
        // Dropping it closes the pipe: the dialog has all it will ever get.
    }

    // Read on a thread, so a child that never answers is a timeout here rather
    // than a server that waits for it forever.
    let (answered, line) = channel();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut first = String::new();
            let _ = BufReader::new(stdout).read_line(&mut first);
            let _ = answered.send(first);
        });
    }
    let answer = line
        .recv_timeout(Duration::from_secs(request.seconds) + Duration::from_secs(1))
        .ok()
        .and_then(|line| Answer::parse(&line));

    let _ = child.kill();
    let _ = child.wait();
    answer.unwrap_or_else(Answer::deny)
}

/// How the dialog is started: this same executable, in its dialog role.
///
/// `POST_CONFIRM_CMD` replaces it, which is how the tests stand a script in
/// for a window.
fn dialog_command() -> Command {
    if let Some(spec) = std::env::var_os("POST_CONFIRM_CMD") {
        let words: Vec<String> = spec
            .to_string_lossy()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if let Some((program, args)) = words.split_first() {
            let mut command = Command::new(program);
            command.args(args);
            return command;
        }
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("post"));
    let mut command = Command::new(exe);
    command.arg("--confirm");
    command
}

/// Where the allowlist lives: `allowlist.toml` in Post's config dir.
pub fn allowlist_path() -> PathBuf {
    post::paths::config_dir().join("allowlist.toml")
}

/// Reads the allowlist. Anything unreadable is treated as an empty one: the
/// safe direction is to ask.
pub fn load(file: &Path) -> BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return BTreeSet::new();
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        eprintln!("post: {} is not valid TOML — ignoring it", file.display());
        return BTreeSet::new();
    };
    table
        .get("allow")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Writes the allowlist back, creating the directory if this is the first one.
pub fn save(file: &Path, allowed: &BTreeSet<String>) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = String::from(
        "# Tools Post may run without asking. Delete a line to be asked again.\nallow = [\n",
    );
    for tool in allowed {
        // Tool names are plain identifiers; quote them and be done.
        text.push_str(&format!("  \"{}\",\n", tool.replace('"', "")));
    }
    text.push_str("]\n");
    std::fs::write(file, text)
}
