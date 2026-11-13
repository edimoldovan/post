//! Who an account writes to: everyone on the To, Cc and Bcc of its Sent mail,
//! with how often and how lately — what an address line offers as a name or
//! an address is typed. Read from Gmail once, then kept up with what was sent
//! since. Rebuildable like the caches: the Sent folder stays the truth.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{gmail, paths, reply, store};

/// How much of the account's allowance — 15,000 units a minute — gathering may
/// spend. A third: the rest is for the window, the terminal and Claude, and
/// for another computer reading the same account.
const UNITS_PER_MINUTE: u32 = 5_000;

/// The pause after each batch that keeps a first read of years of Sent mail
/// at [`UNITS_PER_MINUTE`]: a batch is [`gmail::BATCH`] gets.
const PAUSE: Duration =
    Duration::from_millis(60_000 * (gmail::BATCH as u64) * (gmail::GET_UNITS as u64) / UNITS_PER_MINUTE as u64);

/// Someone the account has written to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    /// The name they were last written to under; empty where none was given.
    pub name: String,
    pub address: String,
    /// How many sent messages had them on.
    pub count: u32,
    /// When they were last written to, in Gmail's milliseconds.
    pub last: i64,
}

impl Person {
    /// What an address line puts on for them: their name and their address.
    pub fn chip(&self) -> String {
        reply::mailbox(&self.name, &self.address)
    }

    /// True when a word of their name or of their address starts with what
    /// is typed, in any case: "bio" finds Alessandro Biolchi, and "acme"
    /// everyone at acme.com.
    pub fn matches(&self, typed: &str) -> bool {
        let typed = typed.trim().to_lowercase();
        !typed.is_empty() && [&self.name, &self.address].iter().any(|text| starts_a_word(&text.to_lowercase(), &typed))
    }
}

/// True when `typed` is in `text` where a word of it starts: at its start, or
/// after anything that is not a letter or a digit.
fn starts_a_word(text: &str, typed: &str) -> bool {
    text.match_indices(typed)
        .any(|(at, _)| text[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric()))
}

/// What is kept on disk for one account.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Kept {
    /// Every sent message counted, so none is counted twice.
    seen: HashSet<String>,
    /// The whole folder has been read: after that, only what is newer is.
    whole: bool,
    /// Most written to first, the latest first among equals.
    people: Vec<Person>,
}

/// One account's people as they are counted, by lower-case address.
#[derive(Debug, Default)]
struct Tally {
    seen: HashSet<String>,
    whole: bool,
    people: HashMap<String, Person>,
}

impl Tally {
    fn of(kept: Kept) -> Tally {
        let people = kept.people.into_iter().map(|person| (person.address.to_lowercase(), person)).collect();
        Tally { seen: kept.seen, whole: kept.whole, people }
    }

    fn kept(&self) -> Kept {
        let mut people: Vec<Person> = self.people.values().cloned().collect();
        people.sort_by(|a, b| b.count.cmp(&a.count).then(b.last.cmp(&a.last)).then(a.address.cmp(&b.address)));
        Kept { seen: self.seen.clone(), whole: self.whole, people }
    }

    /// One sent message, counted once for everyone on it. The name they were
    /// written to under latest is the one they keep.
    fn count(&mut self, id: String, at: i64, to: &str) {
        if !self.seen.insert(id) {
            return;
        }
        let mut on: HashSet<String> = HashSet::new();
        for (name, address) in reply::named(to) {
            let key = address.to_lowercase();
            if !address.contains('@') || !on.insert(key.clone()) {
                continue;
            }
            let person = self.people.entry(key).or_insert_with(|| Person { address, ..Person::default() });
            person.count += 1;
            if at >= person.last {
                person.last = at;
                if !name.is_empty() {
                    person.name = name;
                }
            } else if person.name.is_empty() {
                person.name = name;
            }
        }
    }
}

/// The people `email` has written to, most written to first; none before
/// [`gather`] has got anywhere.
pub fn of(email: &str) -> Vec<Person> {
    load(email).people
}

/// Reads the account's Sent folder for who it writes to, and keeps that. The
/// first time it reads the whole folder, saving after every batch so a stop
/// loses little; after that only until the first message already counted —
/// new mail is on top. A message Gmail did not hand back is asked for again
/// next time. `saved` hears of every save, for whoever shows the people.
/// Blocking: this is what a thread of its own is for.
pub fn gather(email: &str, saved: impl Fn()) -> Result<(), String> {
    let mut tally = Tally::of(load(email));
    let mut page: Option<String> = None;
    let mut missed = false;
    loop {
        let (ids, next) = gmail::sent_ids(email, page.as_deref())?;
        let new: Vec<String> = ids.iter().filter(|id| !tally.seen.contains(*id)).cloned().collect();
        for batch in new.chunks(gmail::BATCH) {
            let sent = gmail::sent_to(email, batch)?;
            missed |= sent.len() < batch.len();
            for (id, at, to) in sent {
                tally.count(id, at, &to);
            }
            save(email, &tally.kept())?;
            saved();
            std::thread::sleep(PAUSE);
        }
        let reached = new.len() < ids.len();
        match next {
            Some(token) if !(tally.whole && reached) => page = Some(token),
            Some(_) => break,
            None => {
                if !missed && !tally.whole {
                    tally.whole = true;
                    save(email, &tally.kept())?;
                }
                break;
            }
        }
    }
    Ok(())
}

fn load(email: &str) -> Kept {
    std::fs::read_to_string(paths::people_file(email))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Writes it whole, through a file beside it, so a stop mid-write leaves the
/// last one standing. An account taken off this computer meanwhile is not
/// written back.
fn save(email: &str, kept: &Kept) -> Result<(), String> {
    if !store::accounts(&paths::accounts_file()).iter().any(|account| account == email) {
        return Err(format!("{email} is not connected"));
    }
    let file = paths::people_file(email);
    let keep = |e: std::io::Error| format!("could not keep {}: {e}", file.display());
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(keep)?;
    }
    let text = serde_json::to_string(kept).map_err(|e| e.to_string())?;
    let part = file.with_extension("json.part");
    std::fs::write(&part, text).map_err(keep)?;
    std::fs::rename(&part, &file).map_err(keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everyone on a sent message is counted once for it, under the name
    /// they were last written to under; a message counted is not counted
    /// again; the most written to come first.
    #[test]
    fn sent_mail_counts_who_it_went_to() {
        let mut tally = Tally::default();
        tally.count("m1".into(), 100, "Ale <ale@acme.com>, cm@ey.com, ALE@acme.com");
        tally.count("m2".into(), 200, "\"Biolchi, Alessandro\" <ale@acme.com>");
        tally.count("m2".into(), 200, "ale@acme.com");
        tally.count("m3".into(), 50, "Old Name <ale@acme.com>, Cristiano <cm@ey.com>");
        let people = tally.kept().people;
        assert_eq!(people.len(), 2);
        assert_eq!((people[0].address.as_str(), people[0].count, people[0].last), ("ale@acme.com", 3, 200));
        assert_eq!(people[0].name, "Biolchi, Alessandro", "the latest name");
        assert_eq!(people[0].chip(), "\"Biolchi, Alessandro\" <ale@acme.com>");
        assert_eq!((people[1].count, people[1].name.as_str()), (2, "Cristiano"), "an older name fills a gap");
        assert!(tally.seen.contains("m3"));
    }

    /// A word of the name or of the address has to start with what is typed.
    #[test]
    fn a_person_is_found_by_the_start_of_any_word() {
        let ale = Person { name: "Alessandro Biolchi".into(), address: "ale.b@acme.com".into(), ..Person::default() };
        for typed in ["ale", "BIO", "alessandro bi", "acme", "b@acme", "ale.b@acme.c"] {
            assert!(ale.matches(typed), "{typed}");
        }
        for typed in ["", " ", "sandro", "cme", "olchi"] {
            assert!(!ale.matches(typed), "{typed}");
        }
    }
}
