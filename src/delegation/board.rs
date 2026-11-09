//! The tasks each conversation's agents handed over while this application
//! runs, as the engine's task hooks report them: what the view's status line,
//! its sidebar and `/tasks` show. A task does not outlive the application:
//! closing it ends every task the engine runs, and a later launch lists it
//! from its room (`history`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use super::spending::Spent;

/// How long a view shows a task's latest notice again whenever it is sent its
/// live events, as a compaction's.
const NOTICE_FOR: Duration = Duration::from_secs(10);

/// How many characters of a task its notice quotes.
const TASK_PREVIEW: usize = 60;

/// How a task ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Completed,
    Cancelled,
    Failed,
    /// Its launch ended without recording how it ended: a crash.
    Interrupted,
}

impl Outcome {
    /// The engine's `task_status` of an ended task.
    pub(crate) fn of(status: &str) -> Self {
        match status {
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            _ => Self::Failed,
        }
    }

    /// How a task its room records ended: one still marked as running ended
    /// with a launch that could not say so.
    pub(crate) fn recorded(status: &str) -> Self {
        match status {
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Interrupted,
        }
    }
}

/// A task an agent handed over, as this application saw it start and end.
#[derive(Clone, Debug)]
pub(crate) struct Task {
    pub(crate) id: String,
    /// The room of the agent that handed it over: a conversation, or the room
    /// of the task that handed it on.
    pub(crate) parent: String,
    /// The worker's room.
    pub(crate) room: String,
    /// The worker's label, else its ID.
    pub(crate) agent: String,
    /// The worker's ID.
    pub(crate) agent_id: String,
    /// Shared: the views copy the tasks they show on every refresh.
    pub(crate) input: Arc<str>,
    pub(crate) started: Instant,
    /// How long it ran and how it ended, once it did.
    pub(crate) ended: Option<(Duration, Outcome)>,
    /// What its worker consumed, read once it ended (`spending`).
    pub(crate) spent: Option<Spent>,
}

impl Task {
    pub(crate) fn running(&self) -> bool {
        self.ended.is_none()
    }

    /// Whether its launch ended before it could record how it ended: what its
    /// last turn used was never recorded.
    pub(crate) fn interrupted(&self) -> bool {
        matches!(self.ended, Some((_, Outcome::Interrupted)))
    }

    /// Its state in a few words: "running 42 s", "completed in 58 s".
    pub(crate) fn state(&self) -> String {
        match self.ended {
            None => format!("running {} s", seconds(self.started.elapsed())),
            Some((took, Outcome::Completed)) => format!("completed in {} s", seconds(took)),
            Some((took, Outcome::Cancelled)) => format!("cancelled after {} s", seconds(took)),
            Some((took, Outcome::Failed)) => format!("failed after {} s", seconds(took)),
            Some((_, Outcome::Interrupted)) => "interrupted".to_owned(),
        }
    }

    /// Its state and what it consumed, once that is known: "completed in
    /// 58 s · 58k tokens · ≈ $0.61".
    pub(crate) fn summary(&self) -> String {
        match &self.spent {
            Some(spent) => format!("{} · {}", self.state(), spent.text()),
            None => self.state(),
        }
    }

    /// The task on one line, cut at [`TASK_PREVIEW`] characters.
    pub(crate) fn preview(&self) -> String {
        preview(&self.input)
    }
}

#[derive(Default)]
struct Board {
    /// Each conversation's latest notice, by the room it is shown in.
    notices: HashMap<String, (Instant, String)>,
    /// In the order they started.
    tasks: Vec<Task>,
    /// The conversations whose earlier tasks were read from the store.
    restored: HashSet<String>,
}

/// The tasks agents handed over, the notices they left, and the `/tasks`
/// panel a view opened on them.
pub(crate) struct Tasks {
    board: Mutex<Board>,
    changes: watch::Sender<()>,
    pub(crate) panel: super::panel::Panel,
}

impl Default for Tasks {
    fn default() -> Self {
        Self {
            board: Mutex::default(),
            changes: watch::channel(()).0,
            panel: super::panel::Panel::default(),
        }
    }
}

impl Tasks {
    /// Records `task` started. A task already recorded stays ended: the hooks
    /// run apart, and a quick task's end can be told first, without the task
    /// it was given, which its start then fills in.
    pub(crate) fn started(&self, task: Task) {
        let mut board = crate::sync::lock(&self.board);
        if let Some(known) = board.tasks.iter_mut().find(|known| known.id == task.id) {
            if known.input.is_empty() {
                known.input = task.input;
            }
            drop(board);
            self.changes.send_replace(());
            return;
        }
        let text = format!("Task for {} started: {}", task.agent, task.preview());
        board
            .notices
            .insert(task.parent.clone(), (Instant::now(), text));
        board.tasks.push(task);
        drop(board);
        self.changes.send_replace(());
    }

    /// Records the task `ended` as it ended: the one recorded under its ID,
    /// else `ended` itself, whose start was not told yet.
    pub(crate) fn ended(&self, ended: Task) {
        let Some((took, outcome)) = ended.ended else {
            return;
        };
        let mut board = crate::sync::lock(&self.board);
        let known = board.tasks.iter().position(|known| known.id == ended.id);
        // An end that says nothing of how long the task ran is measured
        // from the start recorded.
        let took = match known {
            Some(index) if took.is_zero() => board.tasks[index].started.elapsed(),
            _ => took,
        };
        let outcome_text = match outcome {
            Outcome::Completed => format!("completed in {} s", seconds(took)),
            Outcome::Cancelled => "cancelled".to_owned(),
            Outcome::Failed => "failed".to_owned(),
            Outcome::Interrupted => "interrupted".to_owned(),
        };
        let text = format!("Task for {} {outcome_text}", ended.agent);
        board
            .notices
            .insert(ended.parent.clone(), (Instant::now(), text));
        match known {
            Some(index) => board.tasks[index].ended = Some((took, outcome)),
            None => board.tasks.push(ended),
        }
        drop(board);
        self.changes.send_replace(());
    }

    /// Whether the earlier tasks of `room` are yet to be read: true for its
    /// first view alone.
    pub(crate) fn restoring(&self, room: &str) -> bool {
        crate::sync::lock(&self.board)
            .restored
            .insert(room.to_owned())
    }

    /// Lists `tasks`, which earlier launches ran, telling no one: a task the
    /// hooks already reported stays as they told it.
    pub(crate) fn restored(&self, tasks: Vec<Task>) {
        let mut board = crate::sync::lock(&self.board);
        for task in tasks {
            if !board.tasks.iter().any(|known| known.id == task.id) {
                board.tasks.push(task);
            }
        }
    }

    /// Keeps what task `id`'s worker consumed. Nothing changes on screen but
    /// what the view that read it is about to show.
    pub(crate) fn keep_spent(&self, id: &str, spent: Spent) {
        let mut board = crate::sync::lock(&self.board);
        if let Some(task) = board.tasks.iter_mut().find(|task| task.id == id) {
            task.spent = Some(spent);
        }
    }

    /// The latest notice of `room` while it is recent.
    pub(crate) fn notices(&self, room: &str) -> Vec<String> {
        crate::sync::lock(&self.board)
            .notices
            .get(room)
            .filter(|(at, _)| at.elapsed() < NOTICE_FOR)
            .map(|(_, text)| text.clone())
            .into_iter()
            .collect()
    }

    /// The tasks handed over in `room`, and those their workers handed on,
    /// each after the task that handed it on, with how deep it was handed on
    /// (0 for a task of `room`).
    pub(crate) fn of(&self, room: &str) -> Vec<(usize, Task)> {
        let board = crate::sync::lock(&self.board);
        let mut listed = Vec::new();
        descend(&board.tasks, room, 0, &mut listed);
        listed
    }

    /// The task `id` if `room`'s conversation lists it.
    pub(crate) fn find(&self, room: &str, id: &str) -> Option<Task> {
        self.of(room)
            .into_iter()
            .map(|(_, task)| task)
            .find(|task| task.id == id)
    }

    /// How many tasks run, in every conversation: closing the application
    /// ends them all.
    pub(crate) fn running(&self) -> usize {
        crate::sync::lock(&self.board)
            .tasks
            .iter()
            .filter(|task| task.running())
            .count()
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }
}

/// Lists in `listed` the tasks handed over in `room`, each followed by those
/// its worker handed on. The engine refuses a task past a few rooms deep;
/// the bound only guards against a cycle.
fn descend(tasks: &[Task], room: &str, depth: usize, listed: &mut Vec<(usize, Task)>) {
    if depth > crate::delegation::MAX_DEPTH {
        return;
    }
    for task in tasks.iter().filter(|task| task.parent == room) {
        listed.push((depth, task.clone()));
        descend(tasks, &task.room, depth + 1, listed);
    }
}

/// `task` on one line, cut at [`TASK_PREVIEW`] characters.
fn preview(task: &str) -> String {
    let line = task.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= TASK_PREVIEW {
        return line;
    }
    let cut: String = line.chars().take(TASK_PREVIEW - 1).collect();
    format!("{cut}…")
}

/// `took` in whole seconds, at least 1.
fn seconds(took: Duration) -> u64 {
    took.as_secs_f64().round().max(1.0) as u64
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
    use super::*;

    fn task(id: &str, parent: &str, room: &str) -> Task {
        Task {
            id: id.into(),
            parent: parent.into(),
            room: room.into(),
            agent: "Review".into(),
            agent_id: "review".into(),
            input: format!("Task {id}").into(),
            started: Instant::now(),
            ended: None,
            spent: None,
        }
    }

    fn ended(mut task: Task, outcome: Outcome) -> Task {
        task.ended = Some((Duration::from_millis(58_400), outcome));
        task
    }

    #[test]
    fn a_notice_quotes_a_task_on_one_short_line() {
        let long = "Review\n the diff ".repeat(20);
        let line = preview(&long);
        assert_eq!(line.chars().count(), TASK_PREVIEW);
        assert!(line.starts_with("Review the diff Review") && line.ends_with('…'));
        assert_eq!(preview("Review  the\ndiff"), "Review the diff");
        assert_eq!(seconds(Duration::from_millis(1_400)), 1);
        assert_eq!(seconds(Duration::from_millis(200)), 1);
    }

    /// A conversation lists its tasks and those their workers handed on, each
    /// under the task that handed it on; another conversation's stay out.
    #[test]
    fn a_conversation_lists_its_tasks_and_those_handed_on() {
        let tasks = Tasks::default();
        tasks.started(task("a", "main", "main::a"));
        tasks.started(task("other", "second", "second::x"));
        tasks.started(task("b", "main", "main::b"));
        tasks.started(task("a1", "main::a", "main::a::a1"));
        let listed: Vec<(usize, String)> = tasks
            .of("main")
            .into_iter()
            .map(|(depth, task)| (depth, task.id))
            .collect();
        assert_eq!(listed, [(0, "a".into()), (1, "a1".into()), (0, "b".into())]);
        assert!(tasks.find("main", "a1").is_some());
        assert!(tasks.find("main", "other").is_none());
        assert_eq!(tasks.running(), 4);
    }

    /// A task's end replaces its state and leaves a notice; an end told before
    /// its start records the task, and the late start leaves it ended.
    #[test]
    fn a_task_ends_once_whichever_hook_is_told_first() {
        let tasks = Tasks::default();
        tasks.started(task("a", "main", "main::a"));
        assert_eq!(tasks.notices("main"), ["Task for Review started: Task a"]);
        tasks.ended(ended(task("a", "main", "main::a"), Outcome::Completed));
        assert_eq!(tasks.notices("main"), ["Task for Review completed in 58 s"]);
        let a = tasks.find("main", "a").unwrap();
        assert_eq!(a.state(), "completed in 58 s");

        let mut told = ended(task("quick", "main", "main::q"), Outcome::Cancelled);
        told.input = "".into();
        tasks.ended(told);
        tasks.started(task("quick", "main", "main::q"));
        assert_eq!(tasks.notices("main"), ["Task for Review cancelled"]);
        let quick = tasks.find("main", "quick").unwrap();
        assert_eq!(quick.state(), "cancelled after 58 s");
        assert_eq!(&*quick.input, "Task quick");
        assert_eq!(tasks.running(), 0);
    }

    /// A room says how its task ended; a status that is no end means the
    /// launch that ran it could not say so.
    #[test]
    fn a_room_s_status_reads_as_how_its_task_ended() {
        assert_eq!(Outcome::recorded("completed"), Outcome::Completed);
        assert_eq!(Outcome::recorded("failed"), Outcome::Failed);
        assert_eq!(Outcome::recorded("cancelled"), Outcome::Cancelled);
        assert_eq!(Outcome::recorded("in_progress"), Outcome::Interrupted);
        assert_eq!(Outcome::recorded("pending"), Outcome::Interrupted);
    }

    /// What the hooks told stands: a task read back from its room adds only
    /// what they did not.
    #[test]
    fn a_restored_task_never_replaces_one_the_hooks_told() {
        let tasks = Tasks::default();
        tasks.started(task("live", "main", "main::l"));
        tasks.restored(vec![
            ended(task("live", "main", "main::l"), Outcome::Failed),
            ended(task("old", "main", "main::o"), Outcome::Completed),
        ]);
        let listed: Vec<(String, bool)> = tasks
            .of("main")
            .into_iter()
            .map(|(_, task)| (task.id.clone(), task.running()))
            .collect();
        assert_eq!(listed, [("live".into(), true), ("old".into(), false)]);
        assert!(tasks.restoring("main"));
        assert!(!tasks.restoring("main"));
    }
}
