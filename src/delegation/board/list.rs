//! The tasks this application saw, in the order they started: what starts
//! and ends one, and what is asked of them. No lock here: the board holds
//! one for the list and what goes with it.

use std::time::Duration;

use super::task::{Outcome, Task, seconds};
use crate::delegation::spending::Spent;

#[derive(Default)]
pub(super) struct List(Vec<Task>);

impl List {
    fn position(&self, id: &str) -> Option<usize> {
        self.0.iter().position(|known| known.id == id)
    }

    /// Records `task` started, and returns what its conversation is told.
    /// None for a task already recorded, which stays ended: the hooks run
    /// apart, and a quick task's end can be told first, without the task it
    /// was given, which its start then fills in.
    pub(super) fn start(&mut self, task: Task) -> Option<String> {
        if let Some(index) = self.position(&task.id) {
            let known = &mut self.0[index];
            if known.input.is_empty() {
                known.input = task.input;
            }
            return None;
        }
        let text = format!("Task for {} started: {}", task.agent, task.preview());
        self.0.push(task);
        Some(text)
    }

    /// Records the task `ended` as it ended, for `reason` when it failed:
    /// the one recorded under its ID, else `ended` itself, whose start was
    /// not told yet. Returns what its end is told as; none for a task that
    /// does not say how it ended.
    pub(super) fn end(&mut self, ended: Task, reason: Option<&str>) -> Option<String> {
        let (took, outcome) = ended.ended?;
        let known = self.position(&ended.id);
        // An end that says nothing of how long the task ran is measured
        // from the start recorded.
        let took = match known {
            Some(index) if took.is_zero() => self.0[index].started.elapsed(),
            _ => took,
        };
        let text = format!("Task for {} {}", ended.agent, told(took, outcome, reason));
        match known {
            Some(index) => self.0[index].ended = Some((took, outcome)),
            None => self.0.push(ended),
        }
        Some(text)
    }

    /// Lists `tasks`, which earlier launches ran: a task the hooks already
    /// reported stays as they told it.
    pub(super) fn restore(&mut self, tasks: Vec<Task>) {
        for task in tasks {
            if self.position(&task.id).is_none() {
                self.0.push(task);
            }
        }
    }

    /// The room task `id` runs in.
    pub(super) fn room_of(&self, id: &str) -> Option<&str> {
        self.position(id).map(|index| self.0[index].room.as_str())
    }

    /// Keeps what task `id`'s worker consumed.
    pub(super) fn keep_spent(&mut self, id: &str, spent: Spent) {
        if let Some(index) = self.position(id) {
            self.0[index].spent = Some(spent);
        }
    }

    /// A turn was recorded in `room`: what an ended task of that room
    /// consumed, which its agent can still add to, is to be read again.
    pub(super) fn forget_spent_in(&mut self, room: &str) {
        for task in self.0.iter_mut().filter(|t| t.room == room && !t.running()) {
            task.spent = None;
        }
    }

    /// The tasks handed over in `room`, and those their workers handed on,
    /// each after the task that handed it on, with how deep it was handed
    /// on (0 for a task of `room`).
    pub(super) fn of(&self, room: &str) -> Vec<(usize, Task)> {
        let mut listed = Vec::new();
        descend(&self.0, room, 0, &mut listed);
        listed
    }

    /// The conversation `room` belongs to: `room` itself, unless it is a
    /// task's. The depth bound only guards against a cycle.
    pub(super) fn root_of(&self, room: &str) -> String {
        let mut current = room;
        for _ in 0..=crate::delegation::MAX_DEPTH {
            match self.0.iter().find(|task| task.room == current) {
                Some(task) => current = &task.parent,
                None => break,
            }
        }
        current.to_owned()
    }

    /// A running task on the same channel, handed out by the same parent.
    pub(super) fn running_peer(&self, caller: &Task, id: &str) -> Option<Task> {
        let peer = |t: &&Task| {
            t.id == id
                && t.id != caller.id
                && t.parent == caller.parent
                && t.agent_id == caller.agent_id
                && t.running()
        };
        self.0.iter().find(peer).cloned()
    }

    /// The task that runs in `room`, if one does.
    pub(super) fn running_in(&self, room: &str) -> Option<Task> {
        let runs = |task: &&Task| task.room == room && task.running();
        self.0.iter().find(runs).cloned()
    }

    /// Whether `agent` is working on a task right now.
    pub(super) fn agent_running(&self, agent: &str) -> bool {
        self.0
            .iter()
            .any(|task| task.agent_id == agent && task.running())
    }

    /// How many tasks run, in every conversation.
    pub(super) fn running(&self) -> usize {
        self.0.iter().filter(|task| task.running()).count()
    }
}

/// How a task that ran `took` and ended with `outcome` is told, with why it
/// stopped when it failed and the reason says more than that.
fn told(took: Duration, outcome: Outcome, reason: Option<&str>) -> String {
    match outcome {
        Outcome::Completed => format!("completed in {} s", seconds(took)),
        Outcome::Cancelled => "cancelled".to_owned(),
        Outcome::Failed => reason
            .filter(|r| !r.is_empty() && *r != "completed")
            .map_or_else(
                || "failed".to_owned(),
                |r| format!("failed · {}", crate::turn_ends::stop_description(r)),
            ),
        Outcome::Interrupted => "interrupted".to_owned(),
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
