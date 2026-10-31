//! Product-owned policy. The terminal only displays and requests mode changes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::ValueEnum;
use roomkit::{ToolCallEvent, ToolUse};
use roomkit_cli_roomkit::ConversationChannel;
use roomkit_tui::protocol::{Choice, Decision, ModeState};
use serde::Deserialize;

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
        ModeState {
            command: "mode".into(),
            selected: self.mode(room).value().into(),
            choices: vec![
                Choice {
                    value: "ask".into(),
                    label: "Ask".into(),
                    description: "Approve changes, commands, process input and web requests".into(),
                },
                Choice {
                    value: "plan".into(),
                    label: "Plan".into(),
                    description:
                        "Explore and maintain plans; no project changes or command execution".into(),
                },
                Choice {
                    value: "yolo".into(),
                    label: "Yolo".into(),
                    description: "Run selected tools without asking".into(),
                },
            ],
        }
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
        match event.call.name.as_str() {
            "activate_skill"
            | "read_skill_reference"
            | "list"
            | "glob"
            | "grep"
            | "read"
            | "diff"
            | "code_outline"
            | "process_status"
            | "process_read"
            | "process_stop"
            | "output_read"
            | "output_forget" => {
                if review.is_none() {
                    return ToolUse::Allow;
                }
            }
            "plan_create" | "plan_read" | "plan_list" | "plan_update" | "plan_delete" => {
                if review.is_none() {
                    return ToolUse::Allow;
                }
            }
            // The tool's schema defines an omitted dry_run as true.
            "patch"
                if event
                    .call
                    .arguments
                    .get("dry_run")
                    .is_none_or(|value| value == true) =>
            {
                if review.is_none() {
                    return ToolUse::Allow;
                }
            }
            "write" | "edit" | "patch" | "shell" | "process_start" | "process_write"
            | "web_fetch" | "web_search" => {}
            name if name.starts_with("mcp__") => {}
            _ => return ToolUse::block("This tool has no application permission policy."),
        }
        // Changing modes never auto-accepts a pending approval. Recheck Plan
        // after approval; actions already dispatched are not revoked.
        if review.is_none() && self.mode(event.room_id.as_str()) == Mode::Yolo {
            return ToolUse::Allow;
        }
        let Ok(arguments) = serde_json::to_string_pretty(&event.call.arguments) else {
            return ToolUse::block("Could not present the action for approval.");
        };
        let label = match event.call.name.as_str() {
            "shell" | "process_start" => {
                "Command · runs with your user permissions (not sandboxed)"
            }
            "process_write" => "Process input · may trigger further actions in the running command",
            "web_fetch" => "Web request · retrieves this public URL",
            "web_search" => "Web search · sends this query to Exa's anonymous service",
            name if name.starts_with("mcp__") => {
                "MCP server tool · may access external resources or modify data; exact arguments"
            }
            _ => "Proposed workspace change · exact tool arguments",
        };
        let detail = if matches!(event.call.name.as_str(), "shell" | "process_start") {
            let mut parameters = event.call.arguments.clone();
            let command = parameters.remove("command");
            if let Some(command) = command.and_then(|c| c.as_str().map(str::to_owned)) {
                let command = if command.chars().any(|c| c.is_control() && c != '\n') {
                    serde_json::to_string(&command).unwrap_or_default()
                } else {
                    command
                };
                format!(
                    "{label}\n\n$ {command}\n\nOther parameters:\n{}",
                    serde_json::to_string_pretty(&parameters).unwrap_or_default()
                )
            } else {
                format!("{label}:\n{arguments}")
            }
        } else {
            format!("{label}:\n{arguments}")
        };
        let detail = if let Some(reason) = review {
            format!("Hook requires review: {reason}\n\n{detail}")
        } else {
            detail
        };
        let target = ["command", "path", "url", "query", "process_id"]
            .iter()
            .find_map(|key| {
                event
                    .call
                    .arguments
                    .get(*key)
                    .and_then(serde_json::Value::as_str)
            });
        let summary = target.map_or_else(
            || event.call.name.clone(),
            |target| {
                let compact = target.split_whitespace().collect::<Vec<_>>().join(" ");
                let preview: String = compact.chars().take(96).collect();
                format!(
                    "{} · {}{}",
                    event.call.name,
                    preview,
                    if preview.len() < compact.len() {
                        "…"
                    } else {
                        ""
                    }
                )
            },
        );
        if detail.len() > 65_536 {
            return ToolUse::block(
                "The action is too large to review; split it into smaller calls.",
            );
        }
        match channel
            .ask_tool_permission(
                &event,
                format!("Allow {}?", event.call.name),
                detail,
                summary,
                Duration::from_secs(300),
            )
            .await
        {
            Decision::Allow
                if self.mode(event.room_id.as_str()) == Mode::Plan && !plan_allowed(&event) =>
            {
                ToolUse::block(
                    "The mode changed to Plan while approval was pending; this action was not executed.",
                )
            }
            Decision::Allow => ToolUse::Allow,
            Decision::Deny => ToolUse::block(
                "The action was not approved (refused, expired or view disconnected). Do not retry it without a new user instruction.",
            ),
        }
    }
}

fn plan_allowed(event: &ToolCallEvent) -> bool {
    match event.call.name.as_str() {
        "activate_skill" | "read_skill_reference" | "list" | "glob" | "grep" | "read" | "diff" | "code_outline"
        | "process_status" | "process_read" | "output_read"
        | "plan_create" | "plan_read" | "plan_list" | "plan_update" | "plan_delete"
        // Network reads retain Ask's explicit approval, even in Plan mode.
        | "web_fetch" | "web_search" => true,
        "patch" => event.call.arguments.get("dry_run").is_none_or(|v| v == true),
        // No arbitrary shell or unclassified MCP tools, even when named "read".
        _ => false,
    }
}
