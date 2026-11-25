//! The tasks each conversation's agents handed over while this application
//! runs, as the engine's task hooks report them: what the view's status line,
//! its sidebar and `/tasks` show. A task does not outlive the application:
//! closing it ends every task the engine runs, and a later launch lists it
//! from its room (`history`).
//!
//! One lock holds the board, so that a task's start or end, its notice and
//! the figures' version change together. What each part of it does is its
//! own, without a lock: the tasks ([`list`]), and the figures read of
//! them ([`figures`]).

mod figures;
mod list;
mod task;

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use roomkit_tui::protocol::ConversationCost;
use tokio::sync::watch;

use super::spending::Spent;
pub(crate) use task::{Outcome, Task};

/// How long a view shows a task's latest notice again whenever it is sent its
/// live events, as a compaction's.
pub(super) const NOTICE_FOR: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Board {
    /// Each conversation's latest notice, by the room it is shown in.
    notices: HashMap<String, (Instant, String)>,
    /// The tasks, in the order they started.
    tasks: list::List,
    /// The conversations whose earlier tasks were read from the store.
    restored: HashSet<String>,
    /// The agent shown in each conversation, for a view that returns to it.
    leads: HashMap<String, String>,
    /// What tells that a conversation's figures changed, and what was read
    /// of them.
    figures: figures::Figures,
}

impl Board {
    /// Shows `text` in `room`'s status line for a few seconds.
    fn tell(&mut self, room: &str, text: String) {
        self.notices.insert(room.to_owned(), (Instant::now(), text));
    }
}

/// The tasks agents handed over, the notices they left, and the `/tasks`
/// panel a view opened on them.
pub(crate) struct Tasks {
    board: Mutex<Board>,
    changes: watch::Sender<()>,
    pub(crate) panel: super::panel::Panel,
    lines: super::task_lines::TaskLines,
}

impl Default for Tasks {
    fn default() -> Self {
        Self {
            board: Mutex::default(),
            changes: watch::channel(()).0,
            lines: super::task_lines::TaskLines::default(),
            panel: super::panel::Panel::default(),
        }
    }
}

impl Tasks {
    fn board(&self) -> std::sync::MutexGuard<'_, Board> {
        crate::sync::lock(&self.board)
    }

    /// Records `task` started. A task already recorded stays ended
    /// ([`list::List::start`]).
    pub(crate) fn started(&self, task: Task) {
        let room = task.parent.clone();
        {
            let mut board = self.board();
            if let Some(text) = board.tasks.start(task) {
                board.tell(&room, text);
                board.figures.changed();
            }
        }
        self.changes.send_replace(());
    }

    /// Gives the lines of tasks' ends where to be stored.
    pub(crate) fn install_lines(
        &self,
        kit: &roomkit::RoomKit<roomkit_sqlite::SqliteStore>,
        terminal: &roomkit::model::ChannelId,
    ) {
        self.lines.install(kit, terminal);
    }

    /// Records the task `ended`, then leaves its line in the conversation
    /// that asked for it.
    pub(crate) async fn finished(&self, ended: Task, reason: Option<&str>) {
        let (room, id) = (ended.parent.clone(), ended.id.clone());
        let Some(text) = self.ended_with_reason(ended, reason) else {
            return;
        };
        self.lines
            .write(
                &room,
                format!("task-ended-{id}"),
                ("task_ended", serde_json::json!({"task_id":id})),
                &text,
            )
            .await;
        self.changes.send_replace(());
    }

    /// Show the selected machine, agent and model as soon as its turn is submitted.
    pub(crate) async fn assigned(
        &self,
        room: &str,
        task: &str,
        target: serde_json::Value,
        placement: &str,
    ) {
        let text = format!("{task} assigned to {placement}");
        self.lines
            .write(
                room,
                format!("task-placement-{task}"),
                (
                    "task_placement",
                    serde_json::json!({"task_id":task, "executor_target":target}),
                ),
                &text,
            )
            .await;
        self.changes.send_replace(());
    }

    /// Leave the consultation visible beside the parent conversation's placements.
    pub(crate) async fn consultation(&self, parent: &str, task: &str, id: String, text: &str) {
        self.lines
            .write(
                parent,
                id,
                ("task_consultation", serde_json::json!({"task_id":task})),
                text,
            )
            .await;
        self.changes.send_replace(());
    }

    /// The recent task notices in `room`.
    pub(crate) fn task_lines(&self, room: &str) -> Vec<roomkit_tui::protocol::Item> {
        self.lines.recent(room)
    }

    /// Records the task `ended` as it ended: the one recorded under its ID,
    /// else `ended` itself, whose start was not told yet. Returns what its
    /// end is told as.
    pub(crate) fn ended(&self, ended: Task) -> Option<String> {
        self.ended_with_reason(ended, None)
    }

    fn ended_with_reason(&self, ended: Task, reason: Option<&str>) -> Option<String> {
        let room = ended.parent.clone();
        let text = {
            let mut board = self.board();
            let text = board.tasks.end(ended, reason)?;
            board.tell(&room, text.clone());
            board.figures.changed();
            text
        };
        self.changes.send_replace(());
        Some(text)
    }

    /// Whether the earlier tasks of `room` are yet to be read: true for its
    /// first view alone.
    pub(crate) fn restoring(&self, room: &str) -> bool {
        self.board().restored.insert(room.to_owned())
    }

    /// Lists `tasks`, which earlier launches ran, telling no one: a task the
    /// hooks already reported stays as they told it.
    pub(crate) fn restored(&self, tasks: Vec<Task>) {
        self.board().tasks.restore(tasks);
    }

    /// The tasks of `room` could not all be read: its next view tries again.
    pub(crate) fn unread(&self, room: &str) {
        self.board().restored.remove(room);
    }

    /// Where readings of `room`'s figures stand: [`Self::keep_spent`] keeps
    /// one only while no turn was recorded there since.
    pub(crate) fn reading(&self, room: &str) -> u64 {
        self.board().figures.reading(room)
    }

    /// Keeps what task `id`'s worker consumed, read from its room at
    /// `reading`: a reading a turn recorded there since overtook is dropped,
    /// and the next view reads the room again. Nothing changes on screen but
    /// what the view that read it is about to show.
    pub(crate) fn keep_spent(&self, id: &str, spent: Spent, reading: u64) {
        let mut board = self.board();
        let current = board
            .tasks
            .room_of(id)
            .map(|room| board.figures.reading(room));
        if current == Some(reading) {
            board.tasks.keep_spent(id, spent);
        }
    }

    /// Shows `text` in `room`'s status line for a few seconds.
    pub(crate) fn tell(&self, room: &str, text: String) {
        self.board().tell(room, text);
    }

    /// The latest notice of `room` while it is recent.
    pub(crate) fn notices(&self, room: &str) -> Vec<String> {
        let board = self.board();
        let recent = board.notices.get(room);
        let recent = recent.filter(|(at, _)| at.elapsed() < NOTICE_FOR);
        recent.map(|(_, text)| text.clone()).into_iter().collect()
    }

    /// The tasks handed over in `room`, and those their workers handed on,
    /// each after the task that handed it on, with how deep it was handed on
    /// (0 for a task of `room`).
    pub(crate) fn of(&self, room: &str) -> Vec<(usize, Task)> {
        self.board().tasks.of(room)
    }

    /// The conversation `room` belongs to: `room` itself, unless it is a
    /// task's.
    pub(crate) fn root_of(&self, room: &str) -> String {
        self.board().tasks.root_of(room)
    }

    /// The tasks of the conversation `room` belongs to, whichever of its
    /// rooms a view shows.
    pub(crate) fn family(&self, room: &str) -> Vec<(usize, Task)> {
        let board = self.board();
        board.tasks.of(&board.tasks.root_of(room))
    }

    /// The task `id` if the conversation of `room` lists it.
    pub(crate) fn find(&self, room: &str, id: &str) -> Option<Task> {
        self.family(room)
            .into_iter()
            .map(|(_, task)| task)
            .find(|task| task.id == id)
    }

    /// A running task on the same channel, handed out by the same parent.
    pub(crate) fn running_peer(&self, caller: &Task, id: &str) -> Option<Task> {
        self.board().tasks.running_peer(caller, id)
    }

    /// The task that runs in `room`, if one does.
    pub(crate) fn running_in(&self, room: &str) -> Option<Task> {
        self.board().tasks.running_in(room)
    }

    /// The agent working on a task right now, if `agent` is: assembling it
    /// again would replace the loop its turn runs in.
    pub(crate) fn agent_running(&self, agent: &str) -> bool {
        self.board().tasks.agent_running(agent)
    }

    /// Notes the agent a view shows in `room`, for a view returning to it.
    pub(crate) fn viewing(&self, room: &str, agent: &str) {
        let mut board = self.board();
        if board.tasks.root_of(room) == room {
            board.leads.insert(room.to_owned(), agent.to_owned());
        }
    }

    /// A turn recorded in `room`: an ended task of that room, which its agent
    /// can still answer in, is read again, and the conversation's figures
    /// change.
    pub(crate) fn consumed(&self, room: &str) {
        {
            let mut board = self.board();
            board.figures.recorded_in(room);
            board.tasks.forget_spent_in(room);
        }
        self.changes.send_replace(());
    }

    /// The conversation a view of `room` with `agent` belongs to, with the
    /// agent to show there: a view can be in a task's room.
    pub(crate) fn conversation_of(&self, room: &str, agent: &str) -> (String, String) {
        let root = self.root_of(room);
        let lead = self.lead_of(&root).filter(|_| root != room);
        (root, lead.unwrap_or_else(|| agent.to_owned()))
    }

    /// The agent the view of conversation `root` last showed.
    pub(crate) fn lead_of(&self, root: &str) -> Option<String> {
        self.board().leads.get(root).cloned()
    }

    /// Counts what changes a conversation's figures: a task started or
    /// ended, a turn recorded.
    pub(crate) fn version(&self) -> u64 {
        self.board().figures.version()
    }

    /// What conversation `root`'s own agent cost, as read at `version`.
    pub(crate) fn own_cost(&self, root: &str, version: u64) -> Option<Option<ConversationCost>> {
        self.board().figures.own_cost(root, version)
    }

    pub(crate) fn keep_own_cost(&self, root: &str, version: u64, cost: Option<ConversationCost>) {
        self.board().figures.keep_own_cost(root, version, cost);
    }

    /// How many tasks run, in every conversation: closing the application
    /// ends them all.
    pub(crate) fn running(&self) -> usize {
        self.board().tasks.running()
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }
}

#[cfg(test)]
mod tests;
