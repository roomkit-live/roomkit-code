//! Compaction by tiers of an agent's history (HITL a2daedfe): the memory an
//! agent with a known window reads its history through, and the record of
//! each conversation's compactions, kept on disk so that a resumed
//! conversation reads the same summary, its prompt's prefix with it.
//! `/compact` compacts at once, or once the turns under way end, and every
//! compaction leaves a line in its conversation (HITL a2dfba34), and tells
//! its views what it left until the agent's next turn tells better
//! ([`estimate`]).
mod asked;
mod bounds;
mod estimate;
pub(crate) use estimate::Estimate;
mod history;
mod line;
pub(crate) use line::tokens;
mod memory;
mod parts;
mod recorded;
mod runs;
mod store;
pub(crate) use store::Compacted;
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
use roomkit_providers_sqlite::SqliteStore;
use roomkit_tui::protocol::Item;
use std::{
    collections::HashMap,
    path::Path,
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

/// Every conversation's compactions, each in a file of its own in `dir`,
/// and what the agents' slots and the views ask of them.
pub(crate) struct Compactions {
    /// The compactions on disk.
    store: store::Store,
    /// Whose memory compacts.
    bounds: bounds::Bounds,
    /// The compactions asked for, and those that failed.
    asked: asked::Asked,
    /// Where a failed summary's cause is recorded, once the application
    /// gave it.
    diagnostics: std::sync::OnceLock<Arc<crate::diagnostics::Diagnostics>>,
    /// The project snapshot each agent's latest turn read, by room and agent.
    snapshots: Mutex<HashMap<(String, String), String>>,
    /// The latest notice of each room, and when it came.
    notices: Mutex<HashMap<String, (Instant, String)>>,
    /// The compactions running or waiting in each room, and the turns.
    runs: Arc<runs::Runs>,
    /// Where the compactions' lines go.
    journal: line::Journal,
    /// The compactions running outside the turns.
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Records finish before shutdown cancels the remaining summaries.
    writing: tokio::sync::RwLock<()>,
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
            store: store::Store::new(dir),
            bounds: bounds::Bounds::default(),
            asked: asked::Asked::default(),
            diagnostics: std::sync::OnceLock::new(),
            snapshots: Mutex::default(),
            notices: Mutex::default(),
            runs: Arc::new(runs::Runs::new(changes.clone())),
            journal: line::Journal::default(),
            tasks: Mutex::default(),
            writing: tokio::sync::RwLock::default(),
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

    /// What compacts `agent`'s conversation in `room` outside its turns,
    /// where it runs with a configuration of its own.
    pub fn register_in(&self, room: &str, agent: &str, compactor: Arc<dyn Compactor>) {
        self.runs.register_in(room, agent, compactor);
    }

    /// States whether `agent`'s memory compacts, as its agent is assembled.
    pub fn set_bounded(&self, agent: &str, bounded: bool) {
        self.bounds.set(agent, bounded);
    }

    /// States whether `agent`'s memory compacts in `room`, where it runs
    /// with a configuration of its own.
    pub fn set_bounded_in(&self, room: &str, agent: &str, bounded: bool) {
        self.bounds.set_in(room, agent, bounded);
    }

    /// `agent` runs in `room` with its own configuration again.
    pub fn forget_in(&self, room: &str, agent: &str) {
        self.bounds.forget_in(room, agent);
        self.runs.forget_in(room, agent);
    }

    /// Whether `agent`'s memory compacts in `room`.
    pub fn bounded(&self, room: &str, agent: &str) -> bool {
        self.bounds.compacts(room, agent)
    }

    /// `agent`'s compaction in `room`, read from its file the first time.
    pub fn get(&self, room: &str, agent: &str) -> Option<Compacted> {
        self.with((room, agent), |compacted| compacted.clone())
    }

    /// What `change` makes of `agent`'s compaction in `room`. A file that
    /// could not be read as the conversation was first asked about is said,
    /// in the views and the diagnostics: the conversation then starts again
    /// from its whole history.
    fn with<T>(
        &self,
        (room, agent): (&str, &str),
        change: impl FnOnce(&mut Option<Compacted>) -> T,
    ) -> T {
        let (result, unreadable) = self.store.with((room, agent), change);
        if let Some(unreadable) = unreadable {
            self.notice(
                room,
                "This conversation's saved summary could not be read: it starts again from its whole history",
            );
            if let Some(diagnostics) = self.diagnostics.get() {
                let fields = serde_json::json!({"room": room, "set_aside": unreadable.set_aside});
                diagnostics.record("compaction_record_unreadable", fields, None);
            }
        }
        result
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
        local_capacity: Option<u64>,
    ) {
        let Some(_writing) = self.begin_record().await else {
            return;
        };
        let from = compacted.from;
        self.with((room, agent), |kept| *kept = Some(compacted));
        let unsaved = crate::sync::blocking(|| self.store.save(room)).is_err();
        let told = line::Told {
            room,
            agent,
            from,
            summarized,
            sizes,
            local_capacity,
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
        self.capacity_notice((room, agent), notice, sizes.1, local_capacity);
    }

    fn capacity_notice(
        &self,
        (room, agent): (&str, &str),
        notice: &str,
        estimated: usize,
        local_capacity: Option<u64>,
    ) {
        if let Some(warning) = line::capacity_warning(estimated, local_capacity) {
            self.notice(room, &format!("{notice}. {warning}"));
            if let Some(log) = self.diagnostics.get() {
                let fields = serde_json::json!({"room":room, "agent":agent, "estimated_request_tokens": estimated, "context_capacity":local_capacity});
                log.record("compaction_over_capacity", fields, None);
            }
        } else {
            self.notice(room, notice);
        }
    }

    /// Admits a record until shutdown begins, keeping it alive through commit.
    async fn begin_record(&self) -> Option<tokio::sync::RwLockReadGuard<'_, ()>> {
        let writing = self.writing.read().await;
        (!self.closed.load(std::sync::atomic::Ordering::Acquire)).then_some(writing)
    }

    /// Tells the views of `room` `text`, in their status line.
    pub fn notice(&self, room: &str, text: &str) {
        crate::sync::lock(&self.notices).insert(room.into(), (Instant::now(), text.into()));
        self.changes.send_replace(());
    }

    /// A failed summary's cause is recorded in `diagnostics`.
    pub fn report_to(&self, diagnostics: Arc<crate::diagnostics::Diagnostics>) {
        let _ = self.diagnostics.set(diagnostics);
    }

    /// The summary of `agent`'s conversation in `room` failed: the views
    /// say why, the diagnostics record the cause, and the next one waits
    /// ([`asked::Asked::failed`]).
    fn failed(
        &self,
        (room, agent): (&str, &str),
        failure: &summary::Failure,
        local_capacity: Option<u64>,
    ) {
        let wait = self.asked.failed(room, agent);
        self.notice(
            room,
            &format!(
                "Compaction failed: {}",
                failure.recovery(local_capacity, wait)
            ),
        );
        if let Some(diagnostics) = self.diagnostics.get() {
            let fields = serde_json::json!({
                "room": room, "agent": agent, "retry_in_seconds": wait.as_secs(),
            });
            diagnostics.record("compaction_failed", fields, Some(failure.error()));
        }
    }

    /// A summary of `agent`'s conversation in `room` was written.
    fn succeeded(&self, room: &str, agent: &str) {
        self.asked.succeeded(room, agent);
    }

    /// Whether the next summary of `agent`'s conversation in `room` waits
    /// after those that failed.
    fn waits(&self, room: &str, agent: &str) -> bool {
        self.asked.waits(room, agent)
    }

    /// Asks `agent`'s next turn in `room` to compact; false when its memory
    /// does not compact.
    pub fn force(&self, room: &str, agent: &str) -> bool {
        if !self.bounded(room, agent) {
            return false;
        }
        self.asked.force(room, agent);
        true
    }

    /// Whether `agent`'s turn in `room` was asked to compact, which it now
    /// does: no compaction of its waits for the turns to end any more.
    pub fn take_forced(&self, room: &str, agent: &str) -> bool {
        let forced = self.asked.take(room, agent);
        if forced {
            self.runs.unqueue(room, agent);
        }
        forced
    }

    /// `/compact`: compacts `agent`'s conversation in `room` at once, or
    /// once the turns under way end, its next turn compacting it should one
    /// start first.
    pub fn request(self: &Arc<Self>, room: &str, agent: &str) -> Requested {
        if !self.bounded(room, agent) {
            return Requested::Unbounded;
        }
        // Asked by the person: tried now, whatever failed before.
        self.asked.retry_now(room, agent);
        let set = self.asked.force(room, agent);
        match self.runs.ask(room, agent) {
            runs::Asked::Running => {
                // A compaction starting may not have taken its own yet.
                if set {
                    self.asked.take(room, agent);
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
        // starts before the close takes ownership of it, or not at all.
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
        tasks.push(task);
    }

    /// Finishes records before cancelling summaries and closing the engine.
    /// Cancelling a timeline commit partway through can leave a stored row
    /// beyond the delivery cursor and delay the next turn after resuming.
    pub async fn close(&self) {
        {
            let _tasks = crate::sync::lock(&self.tasks);
            self.closed
                .store(true, std::sync::atomic::Ordering::Release);
        }
        let _writing = self.writing.write().await;
        let tasks = std::mem::take(&mut *crate::sync::lock(&self.tasks));
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
    }

    /// Compacts `agent`'s conversation in `room` outside its turns, unless a
    /// turn compacted it meanwhile.
    async fn compact_now(&self, room: &str, agent: &str) {
        let _held = self.hold(room).await;
        if !self.take_forced(room, agent) {
            return;
        }
        let (Some(compactor), Some(kit)) = (self.runs.compactor(room, agent), self.journal.kit())
        else {
            // Asked by the person: it never ends without a word.
            self.notice(
                room,
                "Nothing compacted: this conversation's agent is not ready to compact here",
            );
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

    /// Where `room`'s compactions are kept.
    #[cfg(test)]
    fn path(&self, room: &str) -> std::path::PathBuf {
        self.store.path(room)
    }
}

#[cfg(test)]
mod tests;
