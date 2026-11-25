//! A task an agent handed over, and how it ended.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::delegation::spending::Spent;

/// How many characters of a task its notice quotes.
pub(super) const TASK_PREVIEW: usize = 60;

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

    /// Whether it was stopped or its launch ended before all it used could
    /// be recorded: a request under way may be billed all the same.
    pub(crate) fn cut_short(&self) -> bool {
        matches!(
            self.ended,
            Some((_, Outcome::Interrupted | Outcome::Cancelled))
        )
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

/// `task` on one line, cut at [`TASK_PREVIEW`] characters.
pub(super) fn preview(task: &str) -> String {
    let line = task.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= TASK_PREVIEW {
        return line;
    }
    let cut: String = line.chars().take(TASK_PREVIEW - 1).collect();
    format!("{cut}…")
}

/// `took` in whole seconds, at least 1.
pub(super) fn seconds(took: Duration) -> u64 {
    took.as_secs_f64().round().max(1.0) as u64
}
