//! The compactions on disk: one private file a conversation, read the
//! first time the conversation is asked about and written whole.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::Estimate;

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
    /// The index of the newest event when it was made: a request the agent
    /// sent until then measured the history before the cut.
    #[serde(default)]
    pub at: u64,
    /// What the application recorded of the conversation when the summary
    /// was written, which its message carries beside it: the plans and the
    /// files changed ([`super::recorded`]). None for a compaction made before it
    /// kept them, or with nothing recorded.
    #[serde(default)]
    pub recorded: Option<String>,
    /// What the compaction left, as estimated, for the views to show until
    /// a turn of the agent tells better ([`super::estimate`]). None once one did,
    /// or for a compaction made before it was kept.
    #[serde(default)]
    pub estimate: Option<Estimate>,
    /// What the agent's requests took besides their history when it was
    /// made, the system prompt and the tools: a compaction after a relaunch,
    /// which no turn measured yet, counts it. None when unknown.
    #[serde(default)]
    pub fixed: Option<u64>,
}

/// The most bytes of a conversation's file read: one holding more is no
/// record of compactions.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// One conversation's file: each agent's compaction.
#[derive(Default, Serialize, Deserialize)]
struct Record {
    agents: BTreeMap<String, Compacted>,
}

/// A conversation's file could not be read as it was first asked about.
/// The conversation starts again from its whole history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Unreadable {
    /// Its content was damaged, and the file is kept under another name,
    /// not replaced by the next compaction. One the system refused this
    /// time is left where it is.
    pub set_aside: bool,
}

pub(super) struct Store {
    dir: PathBuf,
    /// The conversations read so far, by room.
    rooms: Mutex<HashMap<String, Record>>,
    /// Held while a room's file is written: one write at a time
    /// ([`Self::save`]).
    saving: Mutex<()>,
}

impl Store {
    pub(super) fn new(dir: &Path) -> Self {
        Self {
            dir: dir.into(),
            rooms: Mutex::default(),
            saving: Mutex::default(),
        }
    }

    /// Where `room`'s file is.
    pub(super) fn path(&self, room: &str) -> PathBuf {
        let digest = crate::digest::sha256_hex(room);
        self.dir.join(format!("{}.json", &digest[..32]))
    }

    /// What `change` makes of the compaction of `agent` in `room`, whose
    /// file is read the first time; and that the file could not be read,
    /// when it is this call that found so.
    pub(super) fn with<T>(
        &self,
        (room, agent): (&str, &str),
        change: impl FnOnce(&mut Option<Compacted>) -> T,
    ) -> (T, Option<Unreadable>) {
        let mut rooms = crate::sync::lock(&self.rooms);
        let mut unreadable = None;
        let record = rooms.entry(room.into()).or_insert_with(|| {
            let (record, found) = crate::sync::blocking(|| self.read(room));
            unreadable = found;
            record
        });
        let mut compacted = record.agents.remove(agent);
        let result = change(&mut compacted);
        if let Some(compacted) = compacted {
            record.agents.insert(agent.into(), compacted);
        }
        (result, unreadable)
    }

    /// `room`'s record; an empty one when it has no file, or one that could
    /// not be read, which is then told.
    fn read(&self, room: &str) -> (Record, Option<Unreadable>) {
        let path = self.path(room);
        let read = || -> std::io::Result<Record> {
            let file = std::fs::File::open(&path)?;
            if file.metadata()?.len() > MAX_BYTES {
                return Err(std::io::Error::other("compaction record too large"));
            }
            Ok(serde_json::from_reader(std::io::BufReader::new(file))?)
        };
        let error = match read() {
            Ok(record) => return (record, None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (Record::default(), None);
            }
            Err(error) => error,
        };
        // Only a file whose content does not read is set aside.
        let damaged = matches!(
            error.kind(),
            std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof
        ) || error.get_ref().is_some();
        let set_aside =
            damaged && std::fs::rename(&path, path.with_extension("json.unreadable")).is_ok();
        (Record::default(), Some(Unreadable { set_aside }))
    }

    /// Writes `room`'s record as it then stands, outside the rooms' lock:
    /// another conversation's turn need not wait for this one's file. One
    /// write at a time, each taking the record as it starts: a copy taken
    /// before another change never lands after that change's own write.
    pub(super) fn save(&self, room: &str) -> std::io::Result<()> {
        let _saving = crate::sync::lock(&self.saving);
        let record = crate::sync::lock(&self.rooms)
            .get(room)
            .map(|record| Record {
                agents: record.agents.clone(),
            });
        record.map_or(Ok(()), |record| self.write(room, &record))
    }

    /// Keeps every save waiting until the returned guard drops.
    #[cfg(test)]
    pub(super) fn hold_saves(&self) -> std::sync::MutexGuard<'_, ()> {
        crate::sync::lock(&self.saving)
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
        crate::storage::write_atomic(&self.path(room), &serde_json::to_vec(record)?)
    }
}
