//! What a person watching an unattended launch from a terminal reads on
//! standard error: where and how it runs, then what the agent says and each
//! tool call as they happen. Decided from the host's events alone: writing it
//! stays with the caller.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use roomkit_tui::protocol::{ActorId, CallId, Event, Item, RunId, Snapshot, ToolCall, ToolState};

use super::driver::without_blank_edges;
use crate::exit_summary::{elapsed, one_line};
use crate::tool_views::plain_label;

/// From this long on, a call that succeeded says so: the person waited for it.
const WORTH_TIMING: Duration = Duration::from_secs(3);

/// A call's identity, which is scoped to its run.
type Key = (RunId, CallId);

pub(super) struct Progress {
    workspace: String,
    provider: Option<String>,
    header_told: bool,
    /// A blank line set what follows apart from the header.
    spaced: bool,
    started: BTreeSet<Key>,
    settled: BTreeSet<Key>,
    /// The agent whose messages show, as the first snapshot names it.
    agent: Option<ActorId>,
    /// The agent's latest message, held until something follows it: the
    /// last one is the answer, which standard output gets alone.
    held: Option<String>,
}

impl Progress {
    pub fn new(workspace: &Path, provider: Option<String>) -> Self {
        Self {
            workspace: workspace.display().to_string(),
            provider,
            header_told: false,
            spaced: false,
            started: BTreeSet::new(),
            settled: BTreeSet::new(),
            agent: None,
            held: None,
        }
    }

    /// The lines `event` adds.
    pub fn on_event(&mut self, event: &Event) -> Vec<String> {
        let lines = match event {
            Event::Snapshot(snapshot) => return self.header(snapshot),
            Event::MessageFinalized { message, .. }
                if Some(&message.actor) == self.agent.as_ref() =>
            {
                let older = self.held.replace(message.text.clone());
                older.map_or_else(Vec::new, |text| message_lines(&text))
            }
            Event::Item {
                item: Item::Tool { run, call, .. },
                ..
            } => match self.call(run, call) {
                Some(line) => {
                    let mut lines = self.release();
                    lines.push(line);
                    lines
                }
                None => return Vec::new(),
            },
            _ => return Vec::new(),
        };
        self.spaced(lines)
    }

    /// The held message, which something followed: it was not the answer.
    fn release(&mut self) -> Vec<String> {
        self.held
            .take()
            .map_or_else(Vec::new, |text| message_lines(&text))
    }

    /// Where and how the launch runs, from the first snapshot alone: a later
    /// one is a refreshed view.
    fn header(&mut self, snapshot: &Snapshot) -> Vec<String> {
        if std::mem::replace(&mut self.header_told, true) {
            return Vec::new();
        }
        self.agent = Some(snapshot.selected_agent.clone());
        let mut settings = Vec::new();
        if let Some(model) = &snapshot.model {
            settings.push(format!("model: {}", model.model));
        }
        if let Some(provider) = &self.provider {
            settings.push(format!("provider: {provider}"));
        }
        if let Some(mode) = &snapshot.mode {
            settings.push(mode_field(&mode.selected));
        }
        settings.push(format!("session: {}", snapshot.conversation.as_str()));
        let name = env!("CARGO_PKG_NAME");
        [format!("{name} · {}", self.workspace), settings.join(" · ")]
            .map(|line| printable(&line))
            .to_vec()
    }

    /// The line a state of `call` adds: one when it starts, so that a long
    /// command does not leave the terminal silent, and one when its end says
    /// something more. The host can send one state several times.
    fn call(&mut self, run: &RunId, call: &ToolCall) -> Option<String> {
        let key = (run.clone(), call.id.clone());
        if !call.state.is_settled() {
            let starts = call.state == ToolState::Running && self.started.insert(key);
            return starts.then(|| line(call, None));
        }
        let unseen = self.started.insert(key.clone());
        if !self.settled.insert(key) {
            return None;
        }
        match outcome(call) {
            Some(outcome) => Some(line(call, Some(&outcome))),
            None => unseen.then(|| line(call, None)),
        }
    }

    /// `lines`, after the blank line that sets the first ones apart.
    fn spaced(&mut self, mut lines: Vec<String>) -> Vec<String> {
        if !lines.is_empty() && !std::mem::replace(&mut self.spaced, true) {
            lines.insert(0, String::new());
        }
        lines
    }
}

/// The selected mode, and what it means when nobody can answer an approval.
fn mode_field(selected: &str) -> String {
    if selected == "yolo" {
        format!("mode: {selected}")
    } else {
        format!("mode: {selected} (approvals refused)")
    }
}

/// A line of `text` without its control characters, which a terminal would
/// read as commands: what the model writes, but also a path or a name.
pub(super) fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// An agent's message as lines a terminal shows as written; a tab, a control
/// character too, becomes spaces so that code keeps its indentation.
fn message_lines(text: &str) -> Vec<String> {
    without_blank_edges(text)
        .lines()
        .map(|line| printable(&line.replace('\t', "    ")))
        .collect()
}

/// `call` on one line. Its arguments and intention come from the model: the
/// line goes to a terminal with no control character left in it.
fn line(call: &ToolCall, outcome: Option<&str>) -> String {
    let label = one_line(&plain_label(call));
    match outcome {
        Some(outcome) => format!("  {label} · {outcome}"),
        None => format!("  {label}"),
    }
}

/// What the end of a settled `call` says, when it says anything.
fn outcome(call: &ToolCall) -> Option<String> {
    let took = call.duration_ms.map(Duration::from_millis);
    // Under a second, the duration would read "0s".
    let timed = |word: &str| match took.filter(|took| took.as_secs() > 0) {
        Some(took) => format!("{word} · {}", elapsed(took)),
        None => word.to_owned(),
    };
    match call.state {
        ToolState::Failed => Some(timed("failed")),
        ToolState::Cancelled => Some(timed("cancelled")),
        ToolState::Denied => Some("refused".to_owned()),
        ToolState::Succeeded => took.filter(|took| *took >= WORTH_TIMING).map(elapsed),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
