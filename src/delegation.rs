//! The engine's delegation as this application serves it (RMK-1532; HITL
//! 2b4cc82b, f7f700bc, a7392f28). Every agent can hand a task to another
//! configured agent with the engine's `delegate_task`: the worker runs in a
//! child room, in the background, and the delegating agent says its result in
//! a turn of its own. `task_status` and `cancel_task` follow and cancel the
//! conversation's tasks. No message of the conversation is handed over: what
//! the worker needs goes in the task and its `context`.
//!
//! A child room takes its conversation's mode, and an action there that needs
//! the person's approval is refused, since no view shows a child room
//! (`permissions::Policy::adopt`). The view's status line says when a task
//! starts and how it ended.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use roomkit::model::{ChannelId, RoomEvent};
use roomkit::tasks::{DelegateHandler, DelegateHandlerOptions};
use roomkit::{HookExecution, HookOptions, HookTrigger, RoomKit};
use roomkit_ai::AiTool;
use roomkit_ai::tasks::{DELEGATE_TOOL, SetupDelegationOptions, setup_delegation};
use roomkit_sqlite::SqliteStore;
use serde_json::Value;
use tokio::sync::watch;

/// The engine's tool that hands a task over.
pub(crate) const DELEGATE: &str = "delegate_task";

/// The argument a model could share the conversation's channels with, which
/// this application refuses: the terminal shared into a child room would list
/// it as a conversation and be delivered the worker's answers.
pub(crate) const SHARE_CHANNELS: &str = "share_channels";

/// What a delegated task's worker reads with its mode at each turn.
pub(crate) const WORKER: &str = "You are working on a task another agent handed you, in a conversation of your own that nobody watches. You cannot ask for approval: an action that needs one is refused. Do what the mode allows, then answer with your result, complete and self-contained: it goes back to the agent that asked, as the task's result.";

/// How many delegated tasks' rooms deep a task can still be handed on from:
/// the conversation's agent, its worker and that worker's worker. Two agents
/// would otherwise hand a task back and forth without end.
pub(crate) const MAX_DEPTH: usize = 3;

/// Why an agent `depth` delegated tasks' rooms deep cannot hand a task on, if
/// it cannot.
pub(crate) fn nesting_refusal(depth: usize) -> Option<String> {
    (depth >= MAX_DEPTH).then(|| {
        "This task was handed on too many times to hand it on again: do the work yourself, or say in your answer what is left.".to_owned()
    })
}

/// How long a view shows a task's latest notice again whenever it is sent its
/// live events, as a compaction's.
const NOTICE_FOR: Duration = Duration::from_secs(10);

/// How many characters of a task its notice quotes.
const TASK_PREVIEW: usize = 60;

/// The agents a task can be handed to, by ID with their labels, as the
/// project's and the global agent files list them now.
#[derive(Default)]
pub(crate) struct Roster(RwLock<Vec<(String, String)>>);

impl Roster {
    pub(crate) fn set(&self, agents: Vec<(String, String)>) {
        *crate::sync::write(&self.0) = agents;
    }

    /// The agents `caller` can hand a task to: every other one.
    pub(crate) fn targets(&self, caller: &str) -> Vec<(String, String)> {
        crate::sync::read(&self.0)
            .iter()
            .filter(|(id, _)| id != caller)
            .cloned()
            .collect()
    }

    /// The label of agent `id`, else its ID.
    fn label(&self, id: &str) -> String {
        crate::sync::read(&self.0)
            .iter()
            .find(|(known, _)| known == id)
            .map_or_else(|| id.to_owned(), |(_, label)| label.clone())
    }

    /// What a turn of `caller` reads of the agents it can delegate to, which
    /// follows the agent files as they change where a served tool's list stays
    /// as it was served; none without another agent.
    pub(crate) fn instruction(&self, caller: &str) -> Option<String> {
        let targets = self.targets(caller);
        if targets.is_empty() {
            return None;
        }
        let listed: Vec<String> = targets
            .iter()
            .map(|(id, label)| format!("{id} ({label})"))
            .collect();
        Some(format!(
            "Agents you can hand a task to with delegate_task, by ID: {}.",
            listed.join(", ")
        ))
    }

    /// Why `caller` cannot hand a task to `target`, if it cannot.
    pub(crate) fn refusal(&self, caller: &str, target: &str) -> Option<String> {
        let targets = self.targets(caller);
        if targets.iter().any(|(id, _)| id == target) {
            return None;
        }
        let ids: Vec<&str> = targets.iter().map(|(id, _)| id.as_str()).collect();
        Some(if ids.is_empty() {
            "No other agent is configured: there is no agent to hand this task to.".to_owned()
        } else {
            format!(
                "Hand a task only to one of these agents: {}.",
                ids.join(", ")
            )
        })
    }
}

/// What the view's status line says of each conversation's latest task.
pub(crate) struct Tasks {
    notices: Mutex<HashMap<String, (Instant, String)>>,
    changes: watch::Sender<()>,
}

impl Default for Tasks {
    fn default() -> Self {
        Self {
            notices: Mutex::default(),
            changes: watch::channel(()).0,
        }
    }
}

impl Tasks {
    fn notice(&self, room: &str, text: String) {
        crate::sync::lock(&self.notices).insert(room.into(), (Instant::now(), text));
        self.changes.send_replace(());
    }

    /// The latest notice of `room` while it is recent.
    pub(crate) fn notices(&self, room: &str) -> Vec<String> {
        crate::sync::lock(&self.notices)
            .get(room)
            .filter(|(at, _)| at.elapsed() < NOTICE_FOR)
            .map(|(_, text)| text.clone())
            .into_iter()
            .collect()
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }
}

/// The handler of every agent's `delegate_task`: the delegating agent is told
/// the result, and no message of the conversation is handed over
/// (`max_conversation` 0, HITL a7392f28).
pub(crate) fn handler(kit: &RoomKit<SqliteStore>) -> DelegateHandler<SqliteStore> {
    DelegateHandler::new(kit, DelegateHandlerOptions::default())
}

/// Serves `delegate_task`, `task_status` and `cancel_task` to the agent
/// `agent`, a channel `kit` holds, which takes the tools a room serves.
pub(crate) fn serve(
    kit: &RoomKit<SqliteStore>,
    handler: &DelegateHandler<SqliteStore>,
    agent: &ChannelId,
) -> crate::Result<()> {
    let options = SetupDelegationOptions { tool: Some(tool()) };
    setup_delegation(agent.clone(), handler, options)?;
    kit.setup_task_tools(agent)?;
    Ok(())
}

/// The three tools as every agent's requests declare them, which its system
/// prompt lists and its context breakdown counts: the engine serves them to
/// the agent's turns, so they are no tool the agent was built with.
pub(crate) fn definitions() -> Vec<AiTool> {
    let engine = |definition: Value| AiTool {
        name: definition["name"].as_str().unwrap_or_default().to_owned(),
        description: definition["description"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        parameters: definition["parameters"].clone(),
        tags: Vec::new(),
    };
    vec![
        tool(),
        engine(roomkit::tasks::task_status_definition()),
        engine(roomkit::tasks::cancel_task_definition()),
    ]
}

/// The engine's generic tool without `share_channels`, its agents listed in
/// each turn's instruction (`Roster::instruction`).
fn tool() -> AiTool {
    let mut tool = DELEGATE_TOOL.clone();
    tool.description = "Hand a task to another agent of this project, which works on it in the \
        background, in a conversation of its own, while you go on. Its result comes back to you \
        in a later message, which you then pass on. Write a complete task: the agent reads none \
        of this conversation, only the task and its context. The agents you can choose are listed \
        in this turn's application instruction."
        .into();
    if let Some(properties) = tool
        .parameters
        .get_mut("properties")
        .and_then(Value::as_object_mut)
    {
        properties.shift_remove(SHARE_CHANNELS);
    }
    tool
}

/// Notes in the parent conversation's status line when a task starts and how
/// it ended, naming its agent by label.
pub(crate) fn install(
    kit: &RoomKit<SqliteStore>,
    tasks: std::sync::Arc<Tasks>,
    roster: std::sync::Arc<Roster>,
) -> crate::Result<()> {
    let (started, ended) = (tasks.clone(), roster.clone());
    kit.hook(
        HookTrigger::OnTaskDelegated,
        move |event: RoomEvent, _| {
            let (tasks, roster) = (started.clone(), roster.clone());
            async move {
                let agent = roster.label(text(&event, "agent_id"));
                let task = preview(text(&event, "task_input"));
                tasks.notice(
                    event.room_id.as_str(),
                    format!("Task for {agent} started: {task}"),
                );
            }
        },
        observer("code-delegation-started"),
    )?;
    kit.hook(
        HookTrigger::OnTaskCompleted,
        move |event: RoomEvent, _| {
            let (tasks, roster) = (tasks.clone(), ended.clone());
            async move {
                let agent = roster.label(text(&event, "agent_id"));
                let outcome = match text(&event, "task_status") {
                    "completed" => {
                        let ms = event.metadata.get("duration_ms").and_then(Value::as_f64);
                        format!("completed in {} s", seconds(ms.unwrap_or_default()))
                    }
                    "cancelled" => "cancelled".to_owned(),
                    _ => "failed".to_owned(),
                };
                tasks.notice(
                    event.room_id.as_str(),
                    format!("Task for {agent} {outcome}"),
                );
            }
        },
        observer("code-delegation-ended"),
    )?;
    Ok(())
}

fn observer(name: &str) -> HookOptions {
    HookOptions {
        execution: HookExecution::Async,
        name: name.into(),
        ..HookOptions::default()
    }
}

/// The text the task event's metadata keeps under `key`, else empty.
fn text<'a>(event: &'a RoomEvent, key: &str) -> &'a str {
    event
        .metadata
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
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

/// `ms` milliseconds in whole seconds, at least 1.
fn seconds(ms: f64) -> u64 {
    let seconds = (ms / 1000.0).round();
    // A float cast saturates; NaN fails the comparison.
    if seconds >= 1.0 { seconds as u64 } else { 1 }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
    use super::*;

    fn roster() -> Roster {
        let roster = Roster::default();
        roster.set(vec![
            ("code".into(), "Code".into()),
            ("review".into(), "Reviewer".into()),
        ]);
        roster
    }

    /// The tool shares no channel of the conversation, and names its agents
    /// in the turn's instruction, which follows the agent files.
    #[test]
    fn the_tool_shares_no_channel_and_names_no_fixed_agent() {
        let tool = tool();
        assert_eq!(tool.name, DELEGATE);
        let properties = tool.parameters["properties"].as_object().unwrap();
        assert!(!properties.contains_key(SHARE_CHANNELS));
        assert!(properties.contains_key("context") && properties.contains_key("task"));
        assert!(properties["agent"].get("enum").is_none());
        assert!(!properties.contains_key("conversation"));
    }

    #[test]
    fn an_agent_hands_a_task_only_to_another_configured_agent() {
        let roster = roster();
        assert_eq!(
            roster.instruction("code").unwrap(),
            "Agents you can hand a task to with delegate_task, by ID: review (Reviewer)."
        );
        assert_eq!(roster.refusal("code", "review"), None);
        assert_eq!(
            roster.refusal("code", "code").unwrap(),
            "Hand a task only to one of these agents: review."
        );
        assert!(roster.refusal("code", "terminal").is_some());
        roster.set(vec![("code".into(), "Code".into())]);
        assert_eq!(roster.instruction("code"), None);
        assert!(
            roster
                .refusal("code", "review")
                .unwrap()
                .starts_with("No other agent")
        );
    }

    /// A worker's worker can still hand a task on; one level deeper cannot,
    /// nor a room whose conversation is unknown.
    #[test]
    fn a_task_is_handed_on_at_most_three_rooms_deep() {
        assert_eq!(nesting_refusal(0), None);
        assert_eq!(nesting_refusal(MAX_DEPTH - 1), None);
        assert!(nesting_refusal(MAX_DEPTH).is_some());
        assert!(nesting_refusal(usize::MAX).is_some());
    }

    #[test]
    fn a_notice_quotes_a_task_on_one_short_line() {
        assert_eq!(preview("Review\n  the   diff"), "Review the diff");
        let long = "word ".repeat(30);
        let cut = preview(&long);
        assert_eq!(cut.chars().count(), TASK_PREVIEW);
        assert!(cut.ends_with('…'));
        assert_eq!(seconds(0.0), 1);
        assert_eq!(seconds(12_400.0), 12);
    }
}
