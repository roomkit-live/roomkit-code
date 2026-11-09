//! Product-owned policy. The terminal displays the mode and requests changes;
//! the user switches it, and the mode tools ask the user to.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use clap::ValueEnum;
use roomkit::ToolCallEvent;
use roomkit::model::HookResult;
use roomkit::model::hook::{AllowOptions, BlockOptions};
use roomkit_tui::protocol::{Choice, Decision, ModeState};
use roomkit_tui_engine::{ConversationChannel, PermissionRequest};
use serde::Deserialize;

mod card;
mod modes;
mod rules;

pub(crate) use modes::{EXIT as PLAN_EXIT, ModeTools, approve_exit};

/// The mode tools' names.
pub(crate) const MODE_TOOLS: [&str; 2] = [modes::ENTER, modes::EXIT];

pub(crate) use rules::summary;

/// What the permission gate decides on a tool call: it runs, runs with other
/// arguments, or is refused with words the model reads.
pub(crate) type Verdict = HookResult<ToolCallEvent>;

/// The call runs.
pub(crate) fn allow() -> Verdict {
    HookResult::allow(AllowOptions::default())
}

/// The call is refused; the model reads `reason`.
pub(crate) fn block(reason: impl Into<String>) -> Verdict {
    HookResult::block(reason, BlockOptions::default())
}

/// The room of `event`; none for a call outside a conversation, which the
/// gate refuses before reading a mode.
pub(crate) fn room_of(event: &ToolCallEvent) -> &str {
    event
        .room_id
        .as_ref()
        .map_or("", roomkit::model::RoomId::as_str)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Mode {
    #[default]
    Ask,
    Plan,
    Yolo,
}

impl Mode {
    fn value(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Plan => "plan",
            Self::Yolo => "yolo",
        }
    }
}

/// What leaving Plan mode reads in a delegated task's room: its review is the
/// conversation's own agent's to ask.
const DELEGATED: &str = "Leaving Plan mode needs the person's review of a plan, which a delegated task cannot ask for: it was not done. Finish what the mode allows and say in your answer what you could not do.";

/// How many rooms up a delegated task's room is followed to its conversation:
/// past it, the chain is taken for a loop and the room for one whose
/// conversation is unknown. The gate stops nesting well before
/// (`delegation::MAX_DEPTH`).
const MAX_DELEGATION_DEPTH: usize = 16;

#[derive(Clone)]
pub(crate) struct Policy {
    initial: Mode,
    modes: Arc<Mutex<BTreeMap<String, Mode>>>,
    /// The room each delegated task's room was opened from (RMK-1532).
    parents: Arc<Mutex<BTreeMap<String, String>>>,
    /// Signals every mode change, so the view's live updates carry the
    /// latest.
    pub changes: Arc<tokio::sync::watch::Sender<()>>,
}

impl Policy {
    pub fn new(initial: Mode) -> Self {
        Self {
            initial,
            modes: Arc::default(),
            parents: Arc::default(),
            changes: Arc::new(tokio::sync::watch::channel(()).0),
        }
    }

    /// Learns from `room`'s metadata whether the engine opened it for a
    /// delegated task, and from which room: its calls then take the mode of
    /// the conversation that room belongs to (HITL f7f700bc). The engine
    /// writes the parent as it creates the room, before its agent runs.
    pub(crate) fn adopt(&self, room: &roomkit::model::Room) {
        let Some(parent) = room.metadata.get("parent_room_id").and_then(|p| p.as_str()) else {
            return;
        };
        crate::sync::lock(&self.parents).insert(room.id.as_str().into(), parent.into());
    }

    /// Whether `room` is a delegated task's.
    pub(crate) fn delegated(&self, room: &str) -> bool {
        crate::sync::lock(&self.parents).contains_key(room)
    }

    /// The conversation `room` belongs to, and how many delegated tasks' rooms
    /// deep `room` is: itself at 0, or for a delegated task's room the
    /// conversation its task was delegated from. None past
    /// `MAX_DELEGATION_DEPTH` rooms.
    fn conversation(&self, room: &str) -> Option<(String, usize)> {
        let parents = crate::sync::lock(&self.parents);
        let mut current = room;
        for depth in 0..=MAX_DELEGATION_DEPTH {
            match parents.get(current) {
                Some(parent) => current = parent,
                None => return Some((current.to_owned(), depth)),
            }
        }
        None
    }

    /// How many delegated tasks' rooms deep `room` is: 0 for a conversation;
    /// `usize::MAX` when its conversation is unknown.
    pub(crate) fn depth(&self, room: &str) -> usize {
        self.conversation(room)
            .map_or(usize::MAX, |(_, depth)| depth)
    }

    /// The mode of `room`'s conversation; Plan, the narrowest, for a room
    /// whose conversation is unknown, rather than the launch mode.
    pub(crate) fn mode(&self, room: &str) -> Mode {
        let Some((conversation, _)) = self.conversation(room) else {
            return Mode::Plan;
        };
        crate::sync::lock(&self.modes)
            .get(&conversation)
            .copied()
            .unwrap_or(self.initial)
    }

    pub fn state(&self, room: &str) -> ModeState {
        ModeState::new(
            "mode",
            self.mode(room).value(),
            vec![
                Choice::new("ask", "Ask")
                    .with_description("Approve changes, commands, process input and web requests"),
                Choice::new("plan", "Plan").with_description(
                    "Explore and maintain plans; no project changes or command execution",
                ),
                Choice::new("yolo", "Yolo").with_description("Run selected tools without asking"),
            ],
        )
    }

    pub(crate) fn instruction(&self, room: &str) -> &'static str {
        match self.mode(room) {
            Mode::Plan => {
                "Application mode: Plan. Respond to the latest user message by exploring and preparing or refining a plan with the available plan tools. Do not implement project changes, execute commands, stop processes, or call MCP tools. Web research still requires approval. Read an existing plan before updating it; list plans first when resuming. Keep concrete steps and declared progress accurate. When the plan is ready, call plan_mode_exit if it is available so the user can approve it; otherwise explain it and wait for the user to switch to Ask or Yolo and request implementation."
            }
            Mode::Ask => {
                "Application mode: Ask. Respond to the latest user message. Changes and execution require the application's approval. Use existing plan tools to track substantial work when useful; read or list the plan before continuing it. A mode change alone is not an instruction to execute a plan."
            }
            Mode::Yolo => {
                "Application mode: Yolo. Respond to the latest user message within its scope; selected tools can run without individual approvals. Use existing plan tools to track substantial work when useful; read or list the plan before continuing it. A mode change alone is not an instruction to execute a plan."
            }
        }
    }

    pub fn set(&self, room: &str, value: &str) -> Result<ModeState, String> {
        let mode = match value {
            "ask" => Mode::Ask,
            "plan" => Mode::Plan,
            "yolo" => Mode::Yolo,
            _ => return Err("Use /mode ask, /mode plan or /mode yolo".into()),
        };
        // Set by the person from a task's room, it is the conversation's; a
        // worker's own mode tools stay on its room, which nothing reads.
        let conversation = self
            .conversation(room)
            .map_or_else(|| room.to_owned(), |(room, _)| room);
        self.apply(&conversation, mode);
        Ok(self.state(room))
    }

    /// Set `room`'s mode, and signal it: the view's live updates carry the
    /// mode, and must not keep one a later change replaced.
    pub(crate) fn apply(&self, room: &str, mode: Mode) {
        crate::sync::lock(&self.modes).insert(room.into(), mode);
        self.changes.send_replace(());
    }

    pub async fn gate(&self, channel: &ConversationChannel, event: ToolCallEvent) -> Verdict {
        self.gate_with_review(channel, event, None, None).await
    }

    pub(crate) fn plan_blocks(&self, event: &ToolCallEvent) -> bool {
        self.mode(room_of(event)) == Mode::Plan && !plan_allowed(event)
    }

    /// The card for `event`, named after the agent that asks when it is a
    /// delegated one's, which any view of its conversation answers.
    fn card(
        &self,
        event: &ToolCallEvent,
        label: &str,
        review: Option<String>,
        asker: Option<String>,
    ) -> Result<PermissionRequest, &'static str> {
        let mut request = card::request(event, label, review)?;
        if let Some(asker) = asker {
            request.title = format!("{asker} asks: {}", request.title);
        }
        Ok(self.answered_in_conversation(room_of(event), request))
    }

    /// `request`, which any view of `room`'s conversation answers, whichever
    /// of its rooms it shows.
    pub(super) fn answered_in_conversation(
        &self,
        room: &str,
        request: PermissionRequest,
    ) -> PermissionRequest {
        match self.conversation(room) {
            Some((conversation, _)) => request.with_group(conversation),
            None => request,
        }
    }

    pub(crate) async fn gate_with_review(
        &self,
        channel: &ConversationChannel,
        event: ToolCallEvent,
        review: Option<String>,
        asker: Option<String>,
    ) -> Verdict {
        if self.plan_blocks(&event) {
            return block(
                "Plan mode: explore and update the plan only. This action is disabled. Ask the user to switch to Ask or Yolo before implementing; do not retry or use another tool to bypass this restriction.",
            );
        }
        let Some(rule) = rules::rule(&event) else {
            return block("This tool has no application permission policy.");
        };
        if rule.direct && review.is_none() {
            return allow();
        }
        // Changing modes never auto-accepts a pending approval. Recheck Plan
        // after approval; actions already dispatched are not revoked.
        if review.is_none() && self.mode(room_of(&event)) == Mode::Yolo {
            return allow();
        }
        let request = match self.card(&event, rule.label, review, asker) {
            Ok(request) => request,
            Err(refusal) => return block(refusal),
        };
        match channel.ask_tool_permission(&event, request).await {
            Decision::Allow
                if self.mode(room_of(&event)) == Mode::Plan && !plan_allowed(&event) =>
            {
                block(
                    "The mode changed to Plan while approval was pending; this action was not executed.",
                )
            }
            Decision::Allow => allow(),
            // Deny, or a decision added by a newer adapter: never execute.
            _ => block(
                "The action was not approved (refused, expired or view disconnected). Do not retry it without a new user instruction.",
            ),
        }
    }
}

fn plan_allowed(event: &ToolCallEvent) -> bool {
    rules::rule(event).is_some_and(|rule| rule.plan)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
    use super::*;
    use roomkit::model::{Room, RoomId};

    /// The room `id` the engine opened for a task delegated from `parent`.
    fn child(id: &str, parent: &str) -> Room {
        let mut room = Room::new(RoomId::new(id).unwrap());
        room.metadata.insert("parent_room_id".into(), parent.into());
        room
    }

    /// RMK-1532: a delegated task's room takes its conversation's mode, at any
    /// depth the chain resolves.
    #[test]
    fn a_delegated_room_takes_its_conversation_s_mode() {
        let policy = Policy::new(Mode::Yolo);
        policy.apply("main", Mode::Ask);
        policy.adopt(&child("main::a", "main"));
        policy.adopt(&child("main::a::b", "main::a"));
        policy.adopt(&Room::new(RoomId::new("other").unwrap()));
        assert_eq!(policy.mode("main::a::b"), Mode::Ask);
        assert_eq!((policy.depth("main"), policy.depth("main::a::b")), (0, 2));
        assert!(policy.delegated("main::a") && !policy.delegated("main"));
        assert!(!policy.delegated("other"));
    }

    /// A person who sets the mode from a task's room sets the conversation's.
    /// A worker's own mode tool cannot: nothing reads its room's mode.
    #[test]
    fn only_the_person_sets_a_conversation_s_mode_from_a_task_s_room() {
        let policy = Policy::new(Mode::Yolo);
        policy.apply("main", Mode::Yolo);
        policy.adopt(&child("main::a", "main"));
        policy.apply("main::a", Mode::Plan);
        assert_eq!(policy.mode("main"), Mode::Yolo);
        assert_eq!(policy.mode("main::a"), Mode::Yolo);
        policy.set("main::a", "ask").unwrap();
        assert_eq!(policy.mode("main"), Mode::Ask);
        assert_eq!(policy.mode("main::a"), Mode::Ask);
    }

    /// A chain deeper than the walk, or a loop, never falls back to the launch
    /// mode: the room is held to Plan.
    #[test]
    fn a_room_whose_conversation_is_unknown_is_held_to_plan() {
        let policy = Policy::new(Mode::Yolo);
        policy.apply("main", Mode::Ask);
        let mut parent = "main".to_owned();
        for level in 0..=MAX_DELEGATION_DEPTH {
            let room = format!("main::{level}");
            policy.adopt(&child(&room, &parent));
            parent = room;
        }
        assert_eq!(policy.mode(&parent), Mode::Plan);
        assert_eq!(policy.depth(&parent), usize::MAX);
        policy.adopt(&child("loop-a", "loop-b"));
        policy.adopt(&child("loop-b", "loop-a"));
        assert_eq!(policy.mode("loop-a"), Mode::Plan);
    }
}
