//! The engine's delegation as this application serves it (RMK-1532; HITL
//! 2b4cc82b, f7f700bc, a7392f28). A conversation's agent can hand a task to
//! another configured agent with the engine's `delegate_task`, and the agent
//! of a task cannot hand it on (HITL d4ede844): the worker runs in a
//! child room, in the background, and the delegating agent says its result in
//! a turn of its own. `task_status` and `cancel_task` follow and cancel the
//! conversation's tasks. No message of the conversation is handed over: what
//! the worker needs goes in the task and its `context`.
//!
//! A child room takes its conversation's mode, and an action there that needs
//! the person's approval asks them in the view they are in (HITL 04faa598).
//! The terminal is shared into each child room, so a view can move into it, follow
//! the worker and write to it (RMK-1535). The view's status line says when a task
//! starts and how it ended; its sidebar lists the conversation's tasks, and
//! `/tasks` shows and stops them (`board`, `panel`), with what each consumed
//! (`spending`).

mod board;
mod ended_line;
pub(crate) mod history;
pub(crate) mod panel;
mod render;
pub(crate) mod spending;
mod transcript;

pub(crate) use board::{Outcome, Task, Tasks};

use std::num::NonZeroUsize;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use roomkit::model::{ChannelId, RoomEvent};
use roomkit::tasks::{DelegateHandler, DelegateHandlerOptions};
use roomkit::{HookExecution, HookOptions, HookTrigger, RoomKit};
use roomkit_ai::AiTool;
use roomkit_ai::tasks::{DELEGATE_TOOL, SetupDelegationOptions, setup_delegation};
use roomkit_sqlite::SqliteStore;
use serde_json::Value;

/// The engine's tool that hands a task over.
pub(crate) const DELEGATE: &str = "delegate_task";

/// The argument a model could share the conversation's channels with, which
/// this application refuses: it shares the terminal itself.
pub(crate) const SHARE_CHANNELS: &str = "share_channels";

/// The argument a model hands the worker what it needs besides the task in.
const CONTEXT: &str = "context";

/// How the tool describes [`CONTEXT`]: models pass it a sentence unless told
/// it takes an object, and the engine refuses the call (seen with
/// ornith-1.5-35b).
const CONTEXT_DESCRIPTION: &str = "Optional. A JSON object, never a string, of what the agent needs besides the task, as named values, for example {\"files\": [\"src/main.rs\"], \"branch\": \"main\"}. It reads it with the task, as data.";

/// What a delegated task's worker reads with its mode at each turn.
pub(crate) const WORKER: &str = "You are working on a task another agent handed you, in a conversation of your own, which the person can open and write in. An action that needs their approval asks them and waits: if it is refused, do not retry it or use another tool to get around it, and say in your answer what you could not do. Do what the mode allows, then answer with your result, complete and self-contained: it goes back to the agent that asked, as the task's result.";

/// What the agent of a task's room reads once its task ended: the person is
/// talking to it.
pub(crate) const AFTER_TASK: &str = "The task another agent handed you has ended and its result went back to that agent. The person is now talking to you directly in this conversation of your own; what you answer stays here.";

/// What the agent of `room` reads about its task, if `room` is a task's.
pub(crate) fn worker_note(room: &roomkit::model::Room) -> Option<&'static str> {
    if !room.metadata.contains_key("parent_room_id") {
        return None;
    }
    let status = room.metadata.get("task_status").and_then(Value::as_str);
    Some(match status {
        Some("pending" | "in_progress") | None => WORKER,
        Some(_) => AFTER_TASK,
    })
}

/// How many delegated tasks' rooms deep a task's room can be, read back: the
/// conversation's agent, its worker and that worker's worker, as earlier
/// launches let them hand a task on.
pub(crate) const MAX_DEPTH: usize = 3;

/// Why an agent `depth` delegated tasks' rooms deep cannot hand a task on,
/// if it cannot: only the conversation's agent hands tasks over. A task a
/// worker handed on came back to it once its own task had ended, so its
/// result never reached the conversation (HITL d4ede844); a worker hands on
/// again once the kit lets it wait for that task.
pub(crate) fn nesting_refusal(depth: usize) -> Option<String> {
    (depth >= 1).then(|| {
        "A task handed to you cannot be handed on: do the work yourself, and say in your answer \
         what you could not do."
            .to_owned()
    })
}

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
    pub(crate) fn label(&self, id: &str) -> String {
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

/// How much of a worker's output comes back to the agent that delegated: the
/// kit's default, 4,000 characters, cut a review short (RMK-1617).
const MAX_RESULT_CHARS: NonZeroUsize = NonZeroUsize::new(20_000).unwrap();

/// The handler of every agent's `delegate_task`: the delegating agent is told
/// the result, and no message of the conversation is handed over
/// (`max_conversation` 0, HITL a7392f28). The terminal is shared into each
/// child room from its start: the engine then streams the worker to it, as it
/// does a conversation's agent, for a view that moves there.
pub(crate) fn handler(kit: &RoomKit<SqliteStore>) -> DelegateHandler<SqliteStore> {
    let terminal = ChannelId::new(crate::application::TERMINAL).ok();
    let options = DelegateHandlerOptions {
        default_share_channels: terminal.map(|terminal| vec![terminal]),
        max_result_chars: MAX_RESULT_CHARS,
        ..DelegateHandlerOptions::default()
    };
    DelegateHandler::new(kit, options)
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
        changes_state: false,
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
        if let Some(context) = properties.get_mut(CONTEXT) {
            context["description"] = CONTEXT_DESCRIPTION.into();
        }
    }
    tool
}

/// Records each task as it starts and ends, naming its agent by label: the
/// parent conversation's status line, sidebar and `/tasks` show it.
pub(crate) fn install(
    kit: &RoomKit<SqliteStore>,
    tasks: std::sync::Arc<Tasks>,
    roster: std::sync::Arc<Roster>,
) -> crate::Result<()> {
    if let Ok(terminal) = ChannelId::new(crate::application::TERMINAL) {
        tasks.install_lines(kit, &terminal);
    }
    let (started, ended) = (tasks.clone(), roster.clone());
    let finishing = tasks.clone();
    kit.hook(
        HookTrigger::OnTaskDelegated,
        move |event: RoomEvent, _| {
            let (tasks, roster) = (started.clone(), roster.clone());
            async move { tasks.started(task(&event, &roster)) }
        },
        observer("code-delegation-started"),
    )?;
    kit.hook(
        HookTrigger::OnTaskCompleted,
        move |event: RoomEvent, _| {
            let (tasks, roster) = (finishing.clone(), ended.clone());
            async move {
                let mut task = task(&event, &roster);
                let ms = event.metadata.get("duration_ms").and_then(Value::as_f64);
                // A start told late leaves no start to measure from.
                let took = ms
                    .and_then(|ms| Duration::try_from_secs_f64(ms / 1000.0).ok())
                    .unwrap_or_default();
                task.ended = Some((took, Outcome::of(text(&event, "task_status"))));
                tasks.finished(task).await;
            }
        },
        observer("code-delegation-ended"),
    )?;
    // A turn's record rides its last row: one that ended on no text is written
    // by updating a row already sent.
    let triggers = [
        (HookTrigger::AfterBroadcast, "code-delegation-consumed"),
        (
            HookTrigger::OnEventUpdated,
            "code-delegation-consumed-update",
        ),
    ];
    for (trigger, name) in triggers {
        let consumed = tasks.clone();
        kit.hook(
            trigger,
            move |event: RoomEvent, _| {
                let tasks = consumed.clone();
                async move {
                    if event.metadata.contains_key("ai_usage")
                        || event.metadata.contains_key("loop_end_reason")
                    {
                        tasks.consumed(event.room_id.as_str());
                    }
                }
            },
            observer(name),
        )?;
    }
    Ok(())
}

/// The task a task hook's `event` reports, as it started. The event is posted
/// in the room of the agent that handed it over.
fn task(event: &RoomEvent, roster: &Roster) -> Task {
    Task {
        id: text(event, "task_id").to_owned(),
        parent: event.room_id.as_str().to_owned(),
        room: text(event, "child_room_id").to_owned(),
        agent: roster.label(text(event, "agent_id")),
        agent_id: text(event, "agent_id").to_owned(),
        input: text(event, "task_input").into(),
        started: Instant::now(),
        ended: None,
        spent: None,
    }
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
        let context = &properties["context"];
        assert_eq!(context["type"], "object");
        let description = context["description"].as_str().unwrap();
        assert!(
            description.contains("JSON object, never a string"),
            "{description}"
        );
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

    /// The conversation's agent hands a task over; the agent of a task, or
    /// of a room whose conversation is unknown, cannot hand it on.
    #[test]
    fn only_the_conversation_s_agent_hands_a_task_over() {
        assert_eq!(nesting_refusal(0), None);
        assert!(nesting_refusal(1).is_some());
        assert!(nesting_refusal(usize::MAX).is_some());
    }
}
