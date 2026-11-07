//! Product-owned policy. The terminal displays the mode and requests changes;
//! the user switches it, and the mode tools ask the user to.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use clap::ValueEnum;
use roomkit::ToolCallEvent;
use roomkit::model::HookResult;
use roomkit::model::hook::{AllowOptions, BlockOptions};
use roomkit_tui::protocol::{Choice, Decision, ModeState};
use roomkit_tui_engine::ConversationChannel;
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

/// What a call that needs an approval reads in a delegated task's room, which
/// no view shows.
const DELEGATED: &str = "This action needs the person's approval, which a delegated task cannot ask for: it was not executed. Do not retry it or use another tool to get around it; finish what the mode allows and say in your answer what you could not do.";

/// How many rooms up a delegated task's room is followed to its conversation:
/// past it, a chain of links is taken for a loop.
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

    /// The conversation `room` belongs to: itself, or for a delegated task's
    /// room the conversation its task was delegated from.
    fn conversation(&self, room: &str) -> String {
        let parents = crate::sync::lock(&self.parents);
        let mut current = room;
        for _ in 0..MAX_DELEGATION_DEPTH {
            match parents.get(current) {
                Some(parent) => current = parent,
                None => break,
            }
        }
        current.to_owned()
    }

    pub(crate) fn mode(&self, room: &str) -> Mode {
        let conversation = self.conversation(room);
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
        self.apply(room, mode);
        Ok(self.state(room))
    }

    /// Set `room`'s mode, and signal it: the view's live updates carry the
    /// mode, and must not keep one a later change replaced.
    pub(crate) fn apply(&self, room: &str, mode: Mode) {
        crate::sync::lock(&self.modes).insert(room.into(), mode);
        self.changes.send_replace(());
    }

    pub async fn gate(&self, channel: &ConversationChannel, event: ToolCallEvent) -> Verdict {
        self.gate_with_review(channel, event, None).await
    }

    pub(crate) fn plan_blocks(&self, event: &ToolCallEvent) -> bool {
        self.mode(room_of(event)) == Mode::Plan && !plan_allowed(event)
    }

    pub(crate) async fn gate_with_review(
        &self,
        channel: &ConversationChannel,
        event: ToolCallEvent,
        review: Option<String>,
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
        if self.delegated(room_of(&event)) {
            return block(DELEGATED);
        }
        let request = match card::request(&event, rule.label, review) {
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
