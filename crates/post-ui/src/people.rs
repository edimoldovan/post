//! Who each account has written to, for the address lines to offer: read off
//! disk, and gathered from Gmail's Sent folder on threads of their own — not
//! as jobs, which the window waits on, because a first gathering takes
//! minutes.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use eframe::egui::Context;
use post::people::Person;

/// How long after one gathering the next one for the same account waits,
/// unless something was sent from it.
const SETTLE: Duration = Duration::from_secs(60);

pub struct People {
    /// Each account's people as last read off disk.
    of: HashMap<String, Vec<Person>>,
    /// The accounts being gathered right now.
    gathering: HashSet<String>,
    /// When each account's last gathering ended.
    ended: HashMap<String, Instant>,
    /// A gathering saved (`false`) or ended (`true`), for an account.
    tx: Sender<(String, bool)>,
    rx: Receiver<(String, bool)>,
}

impl Default for People {
    fn default() -> People {
        let (tx, rx) = channel();
        People { of: HashMap::new(), gathering: HashSet::new(), ended: HashMap::new(), tx, rx }
    }
}

impl People {
    /// The people `account` has written to, most written to first.
    pub fn of(&mut self, account: &str) -> &[Person] {
        self.of.entry(account.to_string()).or_insert_with(|| post::people::of(account))
    }

    /// Gathers what `account` has sent since its last gathering — unless one
    /// is under way, or one ended less than a minute ago.
    pub fn keep_up(&mut self, account: &str, wake: Option<Context>) {
        if self.ended.get(account).is_some_and(|at| at.elapsed() < SETTLE) {
            return;
        }
        self.gather(account, wake);
    }

    /// Gathers what `account` has sent since its last gathering, on a thread
    /// of its own, unless one is under way.
    pub fn gather(&mut self, account: &str, wake: Option<Context>) {
        if account.is_empty() || !self.gathering.insert(account.to_string()) {
            return;
        }
        let (tx, email) = (self.tx.clone(), account.to_string());
        let started = std::thread::Builder::new().name("post-people".into()).spawn(move || {
            let _ = post::people::gather(&email, || {
                let _ = tx.send((email.clone(), false));
            });
            let _ = tx.send((email, true));
            if let Some(ctx) = wake {
                ctx.request_repaint();
            }
        });
        if started.is_err() {
            self.gathering.remove(account);
        }
    }

    /// Takes in what the gatherings saved: those accounts are read off disk
    /// again the next time they are asked for.
    pub fn drain(&mut self) {
        while let Ok((account, ended)) = self.rx.try_recv() {
            self.of.remove(&account);
            if ended {
                self.gathering.remove(&account);
                self.ended.insert(account, Instant::now());
            }
        }
    }
}
