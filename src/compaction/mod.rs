//! Compaction by tiers of an agent's history (HITL a2daedfe): the memory an
//! agent with a known window reads its history through, and the record of
//! each conversation's compactions, kept on disk so that a resumed
//! conversation reads the same summary, its prompt's prefix with it.
mod memory;
mod summary;
mod tiers;

pub(crate) use memory::TieredMemory;
pub(crate) use tiers::ceiling;

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::watch;

/// How many of the room's latest messages a turn's context holds for its
/// memory to read: the kit's ceiling for a memory that does not keep a
/// number of them (roomkit-ai's `DEFAULT_RECENT_EVENTS_WINDOW`).
pub(crate) const WINDOW: usize = 2_000;

/// How long a view shows a room's latest notice again whenever it is sent its
/// live events: one sent once could be replaced before the view read it.
const NOTICE_FOR: Duration = Duration::from_secs(10);

/// A file holding more than this is no record of compactions.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// A conversation's compaction for one agent: where the history it reads
/// whole starts, the summary standing for what came before, and the
/// project's instruction snapshot it carries, which the cut may have left
/// out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Compacted {
    /// The timeline index of the oldest event the agent reads whole.
    pub from: u64,
    pub summary: String,
    pub snapshot: Option<String>,
    /// How many messages the summary stands for, earlier compactions'
    /// included.
    pub summarized: usize,
}

/// One conversation's file: each agent's compaction.
#[derive(Default, Serialize, Deserialize)]
struct Record {
    agents: BTreeMap<String, Compacted>,
}

/// Every conversation's compactions, each in a file of its own in `dir`,
/// and what the agents' slots and the views ask of them.
pub(crate) struct Compactions {
    dir: PathBuf,
    /// The conversations read so far, by room.
    rooms: Mutex<HashMap<String, Record>>,
    /// The agents whose memory compacts, by ID: those with a known window.
    bounded: Mutex<HashSet<String>>,
    /// The conversations whose next turn compacts, by room and agent.
    forced: Mutex<HashSet<(String, String)>>,
    /// The project snapshot each agent's latest turn read, by room and agent.
    snapshots: Mutex<HashMap<(String, String), String>>,
    /// The latest notice of each room, and when it came.
    notices: Mutex<HashMap<String, (Instant, String)>>,
    changes: watch::Sender<()>,
}

impl Compactions {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.into(),
            rooms: Mutex::default(),
            bounded: Mutex::default(),
            forced: Mutex::default(),
            snapshots: Mutex::default(),
            notices: Mutex::default(),
            changes: watch::channel(()).0,
        }
    }

    /// States whether `agent`'s memory compacts, as its agent is assembled.
    pub fn set_bounded(&self, agent: &str, bounded: bool) {
        let mut agents = crate::sync::lock(&self.bounded);
        if bounded {
            agents.insert(agent.into());
        } else {
            agents.remove(agent);
        }
    }

    pub fn bounded(&self, agent: &str) -> bool {
        crate::sync::lock(&self.bounded).contains(agent)
    }

    /// `agent`'s compaction in `room`, read from its file the first time.
    pub fn get(&self, room: &str, agent: &str) -> Option<Compacted> {
        let mut rooms = crate::sync::lock(&self.rooms);
        let record = rooms.entry(room.into()).or_insert_with(|| self.read(room));
        record.agents.get(agent).cloned()
    }

    /// Records `compacted` for `agent` in `room`, in its file first, and
    /// what the views show of it: how many messages it summarized and the
    /// estimated request before and after.
    pub fn record(
        &self,
        (room, agent): (&str, &str),
        compacted: Compacted,
        (before, after): (usize, usize),
    ) {
        let mut notice = format!(
            "Conversation compacted: a summary stands for {} earlier messages · ≈{} → ≈{} tokens",
            compacted.summarized,
            thousands(before),
            thousands(after)
        );
        // Written outside the lock: another conversation's turn need not
        // wait for this one's file.
        let record = {
            let mut rooms = crate::sync::lock(&self.rooms);
            let record = rooms.entry(room.into()).or_insert_with(|| self.read(room));
            record.agents.insert(agent.into(), compacted);
            Record {
                agents: record.agents.clone(),
            }
        };
        if self.write(room, &record).is_err() {
            notice.push_str(" · not saved: a resumed conversation compacts again");
        }
        self.notice(room, &notice);
    }

    /// Tells the views of `room` `text`, in their status line.
    pub fn notice(&self, room: &str, text: &str) {
        crate::sync::lock(&self.notices).insert(room.into(), (Instant::now(), text.into()));
        self.changes.send_replace(());
    }

    /// Asks `agent`'s next turn in `room` to compact; false when its memory
    /// does not compact.
    pub fn force(&self, room: &str, agent: &str) -> bool {
        if !self.bounded(agent) {
            return false;
        }
        crate::sync::lock(&self.forced).insert((room.into(), agent.into()));
        true
    }

    /// Whether `agent`'s turn in `room` was asked to compact, which it now
    /// does.
    pub fn take_forced(&self, room: &str, agent: &str) -> bool {
        crate::sync::lock(&self.forced).remove(&(room.into(), agent.into()))
    }

    /// Keeps the project snapshot `agent`'s turn in `room` read, which a
    /// compaction carries.
    pub fn tell_snapshot(&self, room: &str, agent: &str, snapshot: &str) {
        crate::sync::lock(&self.snapshots).insert((room.into(), agent.into()), snapshot.into());
    }

    pub fn snapshot(&self, room: &str, agent: &str) -> Option<String> {
        crate::sync::lock(&self.snapshots)
            .get(&(room.into(), agent.into()))
            .cloned()
    }

    /// The notice the views of `room` show, while it is recent.
    pub fn notices(&self, room: &str) -> Vec<String> {
        crate::sync::lock(&self.notices)
            .get(room)
            .filter(|(at, _)| at.elapsed() < NOTICE_FOR)
            .map(|(_, text)| text.clone())
            .into_iter()
            .collect()
    }

    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }

    fn path(&self, room: &str) -> PathBuf {
        let digest = crate::digest::sha256_hex(room);
        self.dir.join(format!("{}.json", &digest[..32]))
    }

    /// `room`'s record; none when it has no file, or one that cannot be read,
    /// which the next compaction replaces: the conversation then compacts
    /// again once it passes its tier.
    fn read(&self, room: &str) -> Record {
        let path = self.path(room);
        let read = || -> std::io::Result<Record> {
            let file = std::fs::File::open(&path)?;
            if file.metadata()?.len() > MAX_BYTES {
                return Err(std::io::Error::other("compaction record too large"));
            }
            Ok(serde_json::from_reader(std::io::BufReader::new(file))?)
        };
        read().unwrap_or_default()
    }

    /// Replaces `room`'s file with `record`, readable by its owner alone.
    fn write(&self, room: &str, record: &Record) -> std::io::Result<()> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&self.dir)?;
        let mut temp = tempfile::NamedTempFile::new_in(&self.dir)?;
        temp.write_all(&serde_json::to_vec(record)?)?;
        temp.as_file().sync_all()?;
        temp.persist(self.path(room))?;
        Ok(())
    }
}

/// `tokens` with a comma between thousands: `120,000`.
fn thousands(tokens: usize) -> String {
    let digits = tokens.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
    use super::*;

    fn compacted(from: u64) -> Compacted {
        Compacted {
            from,
            summary: "The user asked for tests.".into(),
            snapshot: Some("Project instruction snapshot".into()),
            summarized: 12,
        }
    }

    #[test]
    fn a_compaction_is_kept_on_disk_privately_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let compactions = Compactions::new(&dir.path().join("compactions"));
        assert_eq!(compactions.get("room", "code"), None);
        compactions.record(("room", "code"), compacted(40), (90_000, 31_000));
        assert_eq!(compactions.get("room", "code"), Some(compacted(40)));
        // Another instance, as after a restart, reads the same.
        let reopened = Compactions::new(&dir.path().join("compactions"));
        assert_eq!(reopened.get("room", "code"), Some(compacted(40)));
        assert_eq!(reopened.get("room", "review"), None);
        assert_eq!(reopened.get("other", "code"), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = compactions.path("room");
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
            let dir = std::fs::metadata(dir.path().join("compactions")).unwrap();
            assert_eq!(dir.permissions().mode() & 0o077, 0);
        }
        let notice = "Conversation compacted: a summary stands for 12 earlier messages · ≈90,000 → ≈31,000 tokens";
        // Every view of the room shows it, each time it is sent its events.
        assert_eq!(compactions.notices("room"), vec![notice.to_owned()]);
        assert_eq!(compactions.notices("room"), vec![notice.to_owned()]);
        assert!(compactions.notices("other").is_empty());
    }

    #[test]
    fn an_unreadable_record_is_none_and_the_next_compaction_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let compactions = Compactions::new(dir.path());
        std::fs::write(compactions.path("room"), "not json").unwrap();
        assert_eq!(compactions.get("room", "code"), None);
        compactions.record(("room", "code"), compacted(7), (1, 1));
        let reopened = Compactions::new(dir.path());
        assert_eq!(reopened.get("room", "code"), Some(compacted(7)));
    }

    #[test]
    fn only_an_agent_whose_memory_compacts_can_be_asked_to() {
        let compactions = Compactions::new(Path::new("/nonexistent"));
        assert!(!compactions.force("room", "code"));
        compactions.set_bounded("code", true);
        assert!(compactions.force("room", "code"));
        assert!(compactions.take_forced("room", "code"));
        assert!(!compactions.take_forced("room", "code"));
        compactions.set_bounded("code", false);
        assert!(!compactions.bounded("code"));
    }
}
