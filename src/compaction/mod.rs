//! Compaction by tiers of an agent's history (HITL a2daedfe): the memory an
//! agent with a known window reads its history through, and the record of
//! each conversation's compactions, kept on disk so that a resumed
//! conversation reads the same summary, its prompt's prefix with it.
//! `/compact` compacts at once, or once the turns under way end, and every
//! compaction leaves a line in its conversation (HITL a2dfba34).
mod history;
mod line;
pub(crate) use line::tokens;
mod memory;
mod recorded;
mod runs;
mod summary;
mod tiers;
mod turns;

pub(crate) use memory::TieredMemory;
pub(crate) use recorded::Recorded;
pub(crate) use runs::Compactor;
pub(crate) use tiers::ceiling;
pub(crate) use turns::lifecycle;

use roomkit::RoomKit;
use roomkit::model::ChannelId;
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::Item;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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
    /// The index of the newest event when it was made: a request the agent
    /// sent until then measured the history before the cut.
    #[serde(default)]
    pub at: u64,
    /// What the application recorded of the conversation when the summary
    /// was written, which its message carries beside it: the plans and the
    /// files changed ([`recorded`]). None for a compaction made before it
    /// kept them, or with nothing recorded.
    #[serde(default)]
    pub recorded: Option<String>,
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
    /// The compactions running or waiting in each room, and the turns.
    runs: Arc<runs::Runs>,
    /// Where the compactions' lines go.
    journal: line::Journal,
    /// The compactions running outside the turns.
    tasks: Mutex<Vec<tokio::task::AbortHandle>>,
    /// Set as the application closes: no compaction starts outside the turns.
    closed: std::sync::atomic::AtomicBool,
    /// What the application records of the conversations, which each summary
    /// carries; none before `carry`.
    recorded: std::sync::OnceLock<Recorded>,
    changes: Arc<watch::Sender<()>>,
}

/// What `/compact` did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Requested {
    /// The agent reads its latest messages: it never compacts.
    Unbounded,
    /// A compaction is already running in the room.
    Running,
    /// A turn is under way: the compaction runs once the room's turns end.
    Queued,
    /// The compaction started.
    Started,
}

impl Compactions {
    pub fn new(dir: &Path) -> Self {
        let changes = Arc::new(watch::channel(()).0);
        Self {
            dir: dir.into(),
            rooms: Mutex::default(),
            bounded: Mutex::default(),
            forced: Mutex::default(),
            snapshots: Mutex::default(),
            notices: Mutex::default(),
            runs: Arc::new(runs::Runs::new(changes.clone())),
            journal: line::Journal::default(),
            tasks: Mutex::default(),
            closed: std::sync::atomic::AtomicBool::new(false),
            recorded: std::sync::OnceLock::new(),
            changes,
        }
    }

    /// The engine whose rooms the compactions read and write their lines
    /// to, from the terminal's channel.
    pub fn install(&self, kit: &RoomKit<SqliteStore>, terminal: &ChannelId) {
        self.journal.install(kit, terminal);
    }

    /// Each summary carries what `recorded` holds of its conversation when
    /// it is written.
    pub fn carry(&self, recorded: Recorded) {
        let _ = self.recorded.set(recorded);
    }

    /// What the application recorded of `room` for `agent`, for a summary
    /// written now.
    async fn recorded(&self, room: &str, agent: &str) -> Option<String> {
        self.recorded.get()?.of(room, agent).await
    }

    /// The lines a summary reads of `agent`'s ended tool calls in `room`
    /// whose index is within `range`. None without an engine, or when its
    /// store cannot be read: the summary then reads the messages alone.
    async fn calls(
        &self,
        (room, agent): (&str, &str),
        range: std::ops::Range<u64>,
    ) -> Vec<(u64, String)> {
        let Some(kit) = self.journal.kit() else {
            return Vec::new();
        };
        history::calls(kit, (room, agent), range)
            .await
            .unwrap_or_default()
    }

    /// What compacts `agent`'s conversations outside its turns.
    pub fn register(&self, agent: &str, compactor: Arc<dyn Compactor>) {
        self.runs.register(agent, compactor);
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
    /// leaves its line in the conversation: how many messages it summarized
    /// anew, the estimated request before and after, how long it `took`, and
    /// whether its summary was `cut` short.
    pub async fn record(
        &self,
        (room, agent): (&str, &str),
        (compacted, summarized): (Compacted, usize),
        (sizes, took, cut): ((usize, usize), Duration, bool),
    ) {
        let from = compacted.from;
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
        let unsaved = self.write(room, &record).is_err();
        let told = line::Told {
            room,
            agent,
            from,
            summarized,
            sizes,
            took,
            unsaved,
            cut,
        };
        self.journal.write(&told).await;
        // A view keeps its status line's last notice: this one replaces the
        // compaction's seconds, the line in the conversation saying the rest.
        let notice = if cut {
            "Conversation compacted; its summary was cut at the model's output limit"
        } else {
            "Conversation compacted"
        };
        self.notice(room, notice);
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
    /// does: no compaction of its waits for the turns to end any more.
    pub fn take_forced(&self, room: &str, agent: &str) -> bool {
        let forced = crate::sync::lock(&self.forced).remove(&(room.into(), agent.into()));
        if forced {
            self.runs.unqueue(room, agent);
        }
        forced
    }

    /// `/compact`: compacts `agent`'s conversation in `room` at once, or
    /// once the turns under way end, its next turn compacting it should one
    /// start first.
    pub fn request(self: &Arc<Self>, room: &str, agent: &str) -> Requested {
        if !self.bounded(agent) {
            return Requested::Unbounded;
        }
        let key = (room.to_owned(), agent.to_owned());
        let set = crate::sync::lock(&self.forced).insert(key.clone());
        match self.runs.ask(room, agent) {
            runs::Asked::Running => {
                // A compaction starting may not have taken its own yet.
                if set {
                    crate::sync::lock(&self.forced).remove(&key);
                }
                Requested::Running
            }
            runs::Asked::Queued => Requested::Queued,
            runs::Asked::Start(running) => {
                self.start(room, agent, running);
                Requested::Started
            }
        }
    }

    /// A turn was admitted in `room`.
    pub fn turn_admitted(&self, room: &str) {
        self.runs.admitted(room);
    }

    /// A turn of `room`'s finished: the compaction that waited for the
    /// turns starts once none is left.
    pub fn turn_finished(self: &Arc<Self>, room: &str) {
        if let Some((agent, running)) = self.runs.finished(room) {
            self.start(room, &agent, running);
        }
    }

    /// Waits for the compaction running in `room`, then keeps the next one
    /// from starting until the returned guard drops.
    pub async fn hold(&self, room: &str) -> tokio::sync::OwnedMutexGuard<()> {
        self.runs.lock(room).lock_owned().await
    }

    /// The views of `room` show a compaction running until the returned
    /// value drops.
    pub fn running(&self, room: &str) -> runs::Running {
        self.runs.running(room)
    }

    /// Compacts `agent`'s conversation in `room` in a task of its own, as
    /// `running` shows.
    fn start(self: &Arc<Self>, room: &str, agent: &str, running: runs::Running) {
        // Taken under the tasks' lock, as `close` sets it: a compaction
        // starts before the close, which aborts it, or not at all.
        let mut tasks = crate::sync::lock(&self.tasks);
        if self.closed.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let (compactions, room, agent) = (self.clone(), room.to_owned(), agent.to_owned());
        let task = tokio::spawn(async move {
            let _running = running;
            compactions.compact_now(&room, &agent).await;
        });
        tasks.retain(|task| !task.is_finished());
        tasks.push(task.abort_handle());
    }

    /// Stops the compactions running outside the turns before the engine
    /// closes, and starts no more: one cut short records nothing, so the
    /// conversation compacts again when it is resumed, or, cut while its line
    /// is stored, keeps its record without its line.
    pub fn close(&self) {
        let mut tasks = crate::sync::lock(&self.tasks);
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        for task in tasks.drain(..) {
            task.abort();
        }
    }

    /// Compacts `agent`'s conversation in `room` outside its turns, unless a
    /// turn compacted it meanwhile.
    async fn compact_now(&self, room: &str, agent: &str) {
        let _held = self.hold(room).await;
        if !self.take_forced(room, agent) {
            return;
        }
        let (Some(compactor), Some(kit)) = (self.runs.compactor(agent), self.journal.kit()) else {
            return;
        };
        let from = self.get(room, agent).map_or(0, |compacted| compacted.from);
        match history::read(kit, room, agent, from).await {
            Some(history) => compactor.compact_now(room, history).await,
            None => {
                self.force(room, agent);
                self.notice(
                    room,
                    "Compaction failed: the conversation could not be read; it is tried again at the next message",
                );
            }
        }
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

    /// The notice the views of `room` show: a compaction running or
    /// waiting, else the latest notice while it is recent.
    pub fn notices(&self, room: &str) -> Vec<String> {
        if let Some(status) = self.runs.status(room) {
            return vec![status];
        }
        crate::sync::lock(&self.notices)
            .get(room)
            .filter(|(at, _)| at.elapsed() < NOTICE_FOR)
            .map(|(_, text)| text.clone())
            .into_iter()
            .collect()
    }

    /// The latest compaction's line in `room`, while it is recent: the views
    /// watching show it at once, the others read it from the conversation.
    pub fn lines(&self, room: &str) -> Option<Item> {
        self.journal.recent(room)
    }

    /// Whether a compaction runs in `room`, whose views count its seconds.
    pub fn compacting(&self, room: &str) -> bool {
        self.runs.compacting(room)
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
            at: from + 9,
            recorded: Some("Recorded by the application: a plan.".into()),
        }
    }

    const TOOK: Duration = Duration::from_secs(3);

    #[tokio::test]
    async fn a_compaction_is_kept_on_disk_privately_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let compactions = Compactions::new(&dir.path().join("compactions"));
        assert_eq!(compactions.get("room", "code"), None);
        compactions
            .record(
                ("room", "code"),
                (compacted(40), 12),
                ((90_000, 31_000), TOOK, false),
            )
            .await;
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
        let text = "Conversation compacted · 12 messages summarized · ≈90k → ≈31k tokens · 3 s";
        // Every view of the room is shown its line, each time it is sent its
        // events, as long as it is recent.
        for _ in 0..2 {
            assert!(
                matches!(compactions.lines("room"), Some(Item::Message(line)) if line.text == text)
            );
        }
        assert!(compactions.lines("other").is_none());
    }

    #[tokio::test]
    async fn an_unreadable_record_is_none_and_the_next_compaction_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let compactions = Compactions::new(dir.path());
        std::fs::write(compactions.path("room"), "not json").unwrap();
        assert_eq!(compactions.get("room", "code"), None);
        compactions
            .record(("room", "code"), (compacted(7), 12), ((1, 1), TOOK, false))
            .await;
        let reopened = Compactions::new(dir.path());
        assert_eq!(reopened.get("room", "code"), Some(compacted(7)));
    }

    /// A second `/compact` while the first has not taken its flag yet leaves
    /// it: the first still compacts.
    #[tokio::test]
    async fn a_compaction_asked_twice_keeps_the_first_one_s_flag() {
        let compactions = Arc::new(Compactions::new(Path::new("/nonexistent")));
        compactions.set_bounded("code", true);
        // The task it spawns runs once this test yields.
        assert_eq!(compactions.request("room", "code"), Requested::Started);
        assert_eq!(compactions.request("room", "code"), Requested::Running);
        assert!(compactions.take_forced("room", "code"));
    }

    /// Once the application closes, a compaction that waited for the turns
    /// starts no more.
    #[tokio::test]
    async fn no_compaction_starts_once_closed() {
        let compactions = Arc::new(Compactions::new(Path::new("/nonexistent")));
        compactions.set_bounded("code", true);
        compactions.turn_admitted("room");
        assert_eq!(compactions.request("room", "code"), Requested::Queued);
        compactions.close();
        compactions.turn_finished("room");
        assert!(crate::sync::lock(&compactions.tasks).is_empty());
        assert!(!compactions.compacting("room"));
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
