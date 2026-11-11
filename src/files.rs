//! Files the agents of a conversation created, modified, deleted or read.
//!
//! File tools report their own effects. A shell command's effects are found by
//! comparing the project before and after it ran; background processes are not
//! followed. Each conversation keeps its list in the conversation store, so it
//! returns with `/resume`.

mod record;
mod snapshot;
mod tools;
mod view;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use roomkit::{ConversationStore, RoomKit, model::RoomId};
use roomkit_sqlite::SqliteStore;
use tokio::sync::{Mutex, OnceCell, watch};

use record::Record;
pub(crate) use record::{Change, Effect};
use snapshot::{Digests, Snapshot};

const STATE_KEY: &str = "roomkit_code:files";

/// One conversation's record, loaded on first use. Its own lock serializes its
/// store reads and writes without holding up other conversations.
type Slot = Arc<Mutex<Option<Record>>>;

pub(crate) struct Files {
    root: PathBuf,
    /// The root as tools may resolve it, to relate absolute paths to it.
    resolved: Option<PathBuf>,
    kit: OnceLock<RoomKit<SqliteStore>>,
    rooms: std::sync::Mutex<HashMap<String, Slot>>,
    digests: Arc<std::sync::Mutex<Digests>>,
    /// Comparisons that outlive their shell call, awaited when closing.
    comparisons: tokio_util::task::TaskTracker,
    /// Set once closing waited for the comparisons.
    settled: OnceCell<()>,
    changes: watch::Sender<()>,
}

impl Files {
    pub fn new(root: &Path) -> Arc<Self> {
        Arc::new(Self {
            root: root.to_path_buf(),
            resolved: std::fs::canonicalize(root).ok(),
            kit: OnceLock::new(),
            rooms: std::sync::Mutex::default(),
            digests: Arc::default(),
            comparisons: tokio_util::task::TaskTracker::new(),
            settled: OnceCell::new(),
            changes: watch::channel(()).0,
        })
    }

    /// Keep records in this conversation store. Before it, they stay in memory.
    pub fn install(&self, kit: &RoomKit<SqliteStore>) {
        let _ = self.kit.set(kit.clone());
    }

    /// Wait, at most `bound`, for comparisons of shell calls that closing the
    /// engine cancelled, so their changes are recorded before the process ends.
    /// Only the first call waits: every agent's slot settles as the engine
    /// closes it, and a comparison that never ends holds up the close once.
    pub async fn settle(&self, bound: std::time::Duration) {
        self.settled
            .get_or_init(|| async {
                self.comparisons.close();
                let _ = tokio::time::timeout(bound, self.comparisons.wait()).await;
            })
            .await;
    }

    /// Compare the project with `before` in a task of its own: dropping the
    /// shell call that started it cannot interrupt it.
    fn compare_later(
        self: &Arc<Self>,
        room: String,
        before: Option<Snapshot>,
    ) -> tokio::task::JoinHandle<()> {
        let files = self.clone();
        self.comparisons.spawn(async move {
            let after = files.snapshot().await;
            files.compared(&room, before, after).await;
        })
    }

    /// Notified whenever a conversation's list changes.
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }

    async fn snapshot(&self) -> Option<Snapshot> {
        let root = self.root.clone();
        let digests = self.digests.clone();
        tokio::task::spawn_blocking(move || {
            let mut digests = crate::sync::lock(&digests);
            snapshot::take(&root, &mut digests)
        })
        .await
        .ok()
        .flatten()
    }

    async fn compared(&self, room: &str, before: Option<Snapshot>, after: Option<Snapshot>) {
        let changes = before
            .zip(after)
            .and_then(|(before, after)| snapshot::compare(&self.root, &before, &after));
        let Some(changes) = changes else {
            self.update(room, |record| record.untracked = true).await;
            return;
        };
        let effects: Vec<_> = changes
            .into_iter()
            .map(|(path, change)| Effect {
                path,
                change,
                shell: true,
            })
            .collect();
        if !effects.is_empty() {
            self.observed(room, effects, None).await;
        }
    }

    async fn observed(&self, room: &str, effects: Vec<Effect>, read: Option<String>) {
        self.update(room, |record| {
            for mut effect in effects {
                effect.path = self.relative(&effect.path);
                record.apply(effect);
            }
            if let Some(path) = read {
                record.read(self.relative(&path));
            }
        })
        .await;
    }

    /// Workspace-relative, as the file tools report it; an absolute path
    /// under the root is made relative. A path outside the workspace stays as
    /// reported.
    fn relative(&self, path: &str) -> String {
        let path = Path::new(path);
        std::iter::once(self.root.as_path())
            .chain(self.resolved.as_deref())
            .find_map(|root| path.strip_prefix(root).ok())
            .map_or_else(|| path.to_string_lossy().into_owned(), snapshot::display)
    }

    fn slot(&self, room: &str) -> Slot {
        crate::sync::lock(&self.rooms)
            .entry(room.to_owned())
            .or_default()
            .clone()
    }

    async fn update(&self, room: &str, change: impl FnOnce(&mut Record)) {
        let slot = self.slot(room);
        let mut held = slot.lock().await;
        if held.is_none() {
            // An unreadable list is never overwritten: keep the new effects
            // in memory only.
            *held = Some(self.load(room).await.unwrap_or_else(Record::offline));
        }
        let Some(record) = held.as_mut() else {
            return;
        };
        change(record);
        if !record.offline {
            record.unsaved = !self.save(room, record).await;
        }
        drop(held);
        self.changes.send_replace(());
    }

    async fn record(&self, room: &str) -> Record {
        let slot = self.slot(room);
        let mut held = slot.lock().await;
        if let Some(record) = held.as_ref() {
            return record.clone();
        }
        // A failed read is retried on the next one instead of being kept.
        let Some(record) = self.load(room).await else {
            return Record::offline();
        };
        *held = Some(record.clone());
        record
    }

    /// The files `room`'s conversation changed, the latest changed first:
    /// each one's net change, marked `shell` when only comparisons around
    /// shell commands found it.
    pub(crate) async fn changes(&self, room: &str) -> Vec<Effect> {
        let record = self.record(room).await;
        let changes = record.changes();
        changes
            .into_iter()
            .map(|changed| Effect {
                path: changed.path.clone(),
                change: changed.change,
                shell: changed.shell,
            })
            .collect()
    }

    /// The stored record, empty when there is none; `None` when it cannot be
    /// read or decoded.
    async fn load(&self, room: &str) -> Option<Record> {
        let (Some(kit), Ok(id)) = (self.kit.get(), RoomId::new(room)) else {
            return Some(Record::default());
        };
        match kit.store().get_private_state(&id, STATE_KEY).await {
            Ok(Some(state)) => serde_json::from_value(serde_json::Value::Object(state)).ok(),
            Ok(None) => Some(Record::default()),
            Err(_) => None,
        }
    }

    /// Whether the record is stored; without a store it only lives in memory.
    async fn save(&self, room: &str, record: &Record) -> bool {
        let Some(kit) = self.kit.get() else {
            return true;
        };
        let (Ok(id), Ok(serde_json::Value::Object(state))) =
            (RoomId::new(room), serde_json::to_value(record))
        else {
            return false;
        };
        kit.store()
            .set_private_state(&id, STATE_KEY, Some(state))
            .await
            .is_ok()
    }
}

#[cfg(test)]
mod tests;
