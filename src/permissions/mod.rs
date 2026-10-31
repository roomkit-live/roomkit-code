//! Product-owned policy. The terminal only displays and requests mode changes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use clap::ValueEnum;
use roomkit::{ToolCallEvent, ToolUse};
use roomkit_tui::protocol::{Choice, Decision, ModeState};
use roomkit_tui_engine::ConversationChannel;
use serde::Deserialize;

mod card;
mod rules;

pub(crate) use rules::summary;

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

#[derive(Clone)]
pub(crate) struct Policy {
    initial: Mode,
    modes: Arc<Mutex<BTreeMap<String, Mode>>>,
}

impl Policy {
    pub fn new(initial: Mode) -> Self {
        Self {
            initial,
            modes: Arc::default(),
        }
    }

    pub(crate) fn mode(&self, room: &str) -> Mode {
        self.modes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(room)
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
                "Application mode: Plan. Respond to the latest user message by exploring and preparing or refining a plan with the available plan tools. Do not implement project changes, execute commands, stop processes, or call MCP tools. Web research still requires approval. Read an existing plan before updating it; list plans first when resuming. Keep concrete steps and declared progress accurate. When the plan is ready, explain it and wait for the user to switch to Ask or Yolo and request implementation. Never switch modes yourself."
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
        self.modes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(room.into(), mode);
        Ok(self.state(room))
    }

    pub async fn gate(&self, channel: &ConversationChannel, event: ToolCallEvent) -> ToolUse {
        self.gate_with_review(channel, event, None).await
    }

    pub(crate) fn plan_blocks(&self, event: &ToolCallEvent) -> bool {
        self.mode(event.room_id.as_str()) == Mode::Plan && !plan_allowed(event)
    }

    pub(crate) async fn gate_with_review(
        &self,
        channel: &ConversationChannel,
        event: ToolCallEvent,
        review: Option<String>,
    ) -> ToolUse {
        if self.plan_blocks(&event) {
            return ToolUse::block(
                "Plan mode: explore and update the plan only. This action is disabled. Ask the user to switch to Ask or Yolo before implementing; do not retry or use another tool to bypass this restriction.",
            );
        }
        let Some(rule) = rules::rule(&event) else {
            return ToolUse::block("This tool has no application permission policy.");
        };
        if rule.direct && review.is_none() {
            return ToolUse::Allow;
        }
        // Changing modes never auto-accepts a pending approval. Recheck Plan
        // after approval; actions already dispatched are not revoked.
        if review.is_none() && self.mode(event.room_id.as_str()) == Mode::Yolo {
            return ToolUse::Allow;
        }
        let request = match card::request(&event, rule.label, review) {
            Ok(request) => request,
            Err(refusal) => return refusal,
        };
        match channel.ask_tool_permission(&event, request).await {
            Decision::Allow
                if self.mode(event.room_id.as_str()) == Mode::Plan && !plan_allowed(&event) =>
            {
                ToolUse::block(
                    "The mode changed to Plan while approval was pending; this action was not executed.",
                )
            }
            Decision::Allow => ToolUse::Allow,
            // Deny, or a decision added by a newer adapter: never execute.
            _ => ToolUse::block(
                "The action was not approved (refused, expired or view disconnected). Do not retry it without a new user instruction.",
            ),
        }
    }
}

fn plan_allowed(event: &ToolCallEvent) -> bool {
    rules::rule(event).is_some_and(|rule| rule.plan)
}
