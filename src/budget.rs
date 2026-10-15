//! Vercel caps a team's observability queries a day, and doesn't say how many
//! are left: past the cap, every query is refused for the rest of the day. So
//! ooze counts its own, across every ooze on this machine, in a file in its
//! cache folder, and keeps the last 24 hours' inside `daily_queries`. When
//! Vercel says the cap is reached, every ooze waits an hour before one tries
//! again.
//!
//! What a query answered is shared the same way: a second ooze, or `ooze
//! watch` beside the view, uses what the first fetched instead of asking again.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

pub const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
/// How long every ooze leaves the API alone after Vercel says the cap is reached.
pub const CAP_PAUSE: i64 = HOUR;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct State {
    /// Queries made, by the hour they were made in.
    pub hours: BTreeMap<i64, u64>,
    /// Vercel said the team's daily cap was reached: no queries until then.
    pub paused_until: Option<i64>,
    pub why: Option<String>,
}

impl State {
    /// Queries made in the last 24 hours.
    pub fn used(&self, now: i64) -> u64 {
        self.hours.range(now - DAY + 1..).map(|(_, n)| n).sum()
    }

    fn prune(&mut self, now: i64) {
        self.hours.retain(|h, _| *h > now - DAY);
    }

    /// When `n` more queries fit in `budget`: now, or when enough of the
    /// oldest are 24 hours old.
    pub fn room_at(&self, n: u64, budget: u64, now: i64) -> i64 {
        let mut over = (self.used(now) + n).saturating_sub(budget);
        if over == 0 {
            return now;
        }
        for (hour, count) in self.hours.range(now - DAY + 1..) {
            over = over.saturating_sub(*count);
            if over == 0 {
                return hour + DAY + HOUR;
            }
        }
        now + DAY
    }
}

/// Why a query can't go now, and until when.
#[derive(Clone, Debug, PartialEq)]
pub enum Wait {
    /// Vercel's daily cap for the team is reached.
    Capped(i64, String),
    /// ooze's own budget is spent until then.
    Budget(i64),
}

fn locked<T>(lock: &Path, f: impl FnOnce() -> T) -> T {
    if let Some(dir) = lock.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(lock).ok();
    if let Some(file) = &file {
        // SAFETY: the descriptor is open for as long as `file` lives.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    }
    // Dropping the file unlocks it.
    f()
}

fn read<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Writes through a temporary file, so a reader never sees half of it.
fn write<T: Serialize>(path: &Path, value: &T) {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if let Ok(bytes) = serde_json::to_vec(value)
        && std::fs::write(&tmp, bytes).is_ok()
    {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[derive(Clone, Debug)]
pub struct Budget {
    dir: PathBuf,
    team: String,
    pub daily: u64,
}

impl Budget {
    pub fn new(dir: &Path, team: &str, daily: u64) -> Budget {
        Budget { dir: dir.to_path_buf(), team: team.to_string(), daily }
    }

    fn file(&self, ext: &str) -> PathBuf {
        self.dir.join(format!("observability-{}.{ext}", self.team))
    }

    pub fn state(&self) -> State {
        read(&self.file("json")).unwrap_or_default()
    }

    /// Takes `n` queries from the budget, or says how long to wait.
    pub fn take(&self, n: u64, now: i64) -> Result<(), Wait> {
        let file = self.file("json");
        locked(&self.file("lock"), || {
            let mut s: State = read(&file).unwrap_or_default();
            if let Some(until) = s.paused_until.filter(|u| *u > now) {
                return Err(Wait::Capped(until, s.why.clone().unwrap_or_default()));
            }
            let at = s.room_at(n, self.daily, now);
            if at > now {
                return Err(Wait::Budget(at));
            }
            s.prune(now);
            *s.hours.entry(now - now % HOUR).or_default() += n;
            s.paused_until = None;
            write(&file, &s);
            Ok(())
        })
    }

    /// Vercel says the cap is reached: every ooze waits before trying again.
    pub fn pause(&self, now: i64, why: &str) -> i64 {
        let file = self.file("json");
        locked(&self.file("lock"), || {
            let mut s: State = read(&file).unwrap_or_default();
            let until = now + CAP_PAUSE;
            s.paused_until = Some(until);
            s.why = Some(why.to_string());
            write(&file, &s);
            until
        })
    }

    /// An answer a query gave, kept for every ooze: `(when it was fetched, what it said)`.
    pub fn shared<T: DeserializeOwned>(&self, name: &str) -> Option<(i64, T)> {
        read(&self.dir.join(format!("{name}-{}.json", self.team)))
    }

    /// The answer another ooze fetched in the last `fresh` milliseconds, or a
    /// new one from `fetch`, which is shared in turn. Only one ooze fetches at
    /// a time; the others wait for it and use its answer.
    pub fn fetch_or_share<T: Serialize + DeserializeOwned, E>(
        &self,
        name: &str,
        fresh: i64,
        now: impl Fn() -> i64,
        fetch: impl FnOnce() -> Result<T, E>,
    ) -> Result<(i64, T), E> {
        let file = self.dir.join(format!("{name}-{}.json", self.team));
        locked(&self.dir.join(format!("{name}-{}.lock", self.team)), || {
            if let Some((at, value)) = read::<(i64, T)>(&file).filter(|(at, _)| now() - at < fresh) {
                return Ok((at, value));
            }
            let value = fetch()?;
            let at = now();
            let _ = std::fs::create_dir_all(&self.dir);
            write(&file, &(at, &value));
            Ok((at, value))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_792_086_000_000;

    #[test]
    fn the_budget_counts_the_last_24_hours_and_says_when_theres_room() {
        let dir = tempfile::tempdir().unwrap();
        let b = Budget::new(dir.path(), "team_1", 10);
        for _ in 0..4 {
            b.take(2, NOW - 20 * HOUR).unwrap();
        }
        b.take(2, NOW).unwrap();
        assert_eq!(b.state().used(NOW), 10);
        let wait = b.take(2, NOW).unwrap_err();
        let old = NOW - 20 * HOUR;
        assert_eq!(wait, Wait::Budget(old - old % HOUR + DAY + HOUR), "room again once the oldest hour is a day old");
        assert!(b.take(2, NOW + 5 * HOUR).is_ok(), "the next day it's free again");
    }

    #[test]
    fn a_pause_holds_every_ooze_until_it_ends() {
        let dir = tempfile::tempdir().unwrap();
        let view = Budget::new(dir.path(), "team_1", 600);
        let watch = Budget::new(dir.path(), "team_1", 600);
        let until = view.pause(NOW, "You have reached the daily observability metrics query limit for your team");
        assert!(
            matches!(watch.take(1, NOW + 60_000), Err(Wait::Capped(u, why)) if u == until && why.contains("daily"))
        );
        assert!(watch.take(1, until + 1).is_ok(), "an hour on, one tries again");
        assert_eq!(watch.state().paused_until, None);
    }

    #[test]
    fn a_fresh_answer_is_shared_instead_of_fetched_again() {
        let dir = tempfile::tempdir().unwrap();
        let a = Budget::new(dir.path(), "team_1", 600);
        let b = Budget::new(dir.path(), "team_1", 600);
        let first: Result<(i64, Vec<u64>), ()> = a.fetch_or_share("live", 60_000, || NOW, || Ok(vec![1, 2, 3]));
        assert_eq!(first.unwrap(), (NOW, vec![1, 2, 3]));
        let mut asked = false;
        let second: Result<(i64, Vec<u64>), ()> = b.fetch_or_share(
            "live",
            60_000,
            || NOW + 30_000,
            || {
                asked = true;
                Ok(vec![9])
            },
        );
        assert_eq!(second.unwrap(), (NOW, vec![1, 2, 3]));
        assert!(!asked, "half a minute old is fresh enough");
        let third: Result<(i64, Vec<u64>), ()> = b.fetch_or_share("live", 60_000, || NOW + 90_000, || Ok(vec![4]));
        assert_eq!(third.unwrap(), (NOW + 90_000, vec![4]));
        assert_eq!(a.shared::<Vec<u64>>("live"), Some((NOW + 90_000, vec![4])));
        let failed: Result<(i64, Vec<u64>), &str> = b.fetch_or_share("live", 1, || NOW + 200_000, || Err("capped"));
        assert_eq!(failed, Err("capped"));
        assert_eq!(a.shared::<Vec<u64>>("live"), Some((NOW + 90_000, vec![4])), "a failure leaves the last answer");
    }
}
