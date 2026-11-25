//! The compactions asked for and those that failed: which conversation's
//! next turn compacts, and when a summary that failed may be tried again.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What the second failed summary in a row waits before the next.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// The longest a summary waits after those that failed.
const RETRY_AT_MOST: Duration = Duration::from_secs(15 * 60);

/// What the next summary waits after `failures` in a row: nothing after
/// the first, then twice as long each time, from [`RETRY_AFTER`] up to
/// [`RETRY_AT_MOST`]. A summary is a request of the history's size, and a
/// refused credential or a history past the model's window fails the same
/// way every time.
fn retry_wait(failures: u32) -> Duration {
    match failures {
        0 | 1 => Duration::ZERO,
        more => RETRY_AFTER
            .saturating_mul(2u32.saturating_pow(more - 2))
            .min(RETRY_AT_MOST),
    }
}

/// A conversation and one of its agents.
type Key = (String, String);

#[derive(Default)]
pub(super) struct Asked {
    /// The conversations whose next turn compacts.
    forced: Mutex<HashSet<Key>>,
    /// The summaries that failed in a row: how many, and when the next may
    /// be tried.
    failures: Mutex<HashMap<Key, (u32, Instant)>>,
}

impl Asked {
    /// Asks `agent`'s next turn in `room` to compact; false when it was
    /// asked already.
    pub(super) fn force(&self, room: &str, agent: &str) -> bool {
        crate::sync::lock(&self.forced).insert((room.into(), agent.into()))
    }

    /// Whether `agent`'s turn in `room` was asked to compact: it is asked
    /// no more.
    pub(super) fn take(&self, room: &str, agent: &str) -> bool {
        crate::sync::lock(&self.forced).remove(&(room.into(), agent.into()))
    }

    /// A summary of `agent`'s conversation in `room` failed: what the next
    /// one waits.
    pub(super) fn failed(&self, room: &str, agent: &str) -> Duration {
        let mut failures = crate::sync::lock(&self.failures);
        let (count, until) = failures
            .entry((room.into(), agent.into()))
            .or_insert((0, Instant::now()));
        *count += 1;
        let wait = retry_wait(*count);
        *until = Instant::now() + wait;
        wait
    }

    /// A summary of `agent`'s conversation in `room` was written.
    pub(super) fn succeeded(&self, room: &str, agent: &str) {
        crate::sync::lock(&self.failures).remove(&(room.into(), agent.into()));
    }

    /// Whether the next summary of `agent`'s conversation in `room` waits
    /// after those that failed.
    pub(super) fn waits(&self, room: &str, agent: &str) -> bool {
        crate::sync::lock(&self.failures)
            .get(&(room.into(), agent.into()))
            .is_some_and(|(_, until)| Instant::now() < *until)
    }

    /// The person asked: the next summary is tried now, whatever failed
    /// before.
    pub(super) fn retry_now(&self, room: &str, agent: &str) {
        let mut failures = crate::sync::lock(&self.failures);
        if let Some((_, until)) = failures.get_mut(&(room.into(), agent.into())) {
            *until = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_failure_waits_nothing_and_the_next_ones_longer_each_time() {
        let waits: Vec<u64> = (1..=8).map(|n| retry_wait(n).as_secs()).collect();
        assert_eq!(waits, [0, 60, 120, 240, 480, 900, 900, 900]);
        let asked = Asked::default();
        assert!(asked.failed("room", "code").is_zero());
        assert!(!asked.waits("room", "code"));
        assert_eq!(asked.failed("room", "code"), RETRY_AFTER);
        assert!(asked.waits("room", "code") && !asked.waits("room", "other"));
        asked.retry_now("room", "code");
        assert!(!asked.waits("room", "code"));
        // The count stands until a summary is written.
        assert_eq!(asked.failed("room", "code"), RETRY_AFTER * 2);
        asked.succeeded("room", "code");
        assert!(asked.failed("room", "code").is_zero());
    }

    #[test]
    fn a_turn_asked_to_compact_is_asked_once() {
        let asked = Asked::default();
        assert!(asked.force("room", "code"));
        assert!(!asked.force("room", "code"));
        assert!(asked.take("room", "code"));
        assert!(!asked.take("room", "code"));
    }
}
