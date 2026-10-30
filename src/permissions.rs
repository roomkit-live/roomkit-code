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
    Yolo,
}

impl Mode {
    fn value(self) -> &'static str {
        match self {
            Self::Ask => "ask",
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

    fn mode(&self, room: &str) -> Mode {
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
                    value: "yolo".into(),
                    label: "Yolo".into(),
                    description: "Run selected tools without asking".into(),
                },
            ],
        }
    }

    pub fn set(&self, room: &str, value: &str) -> Result<ModeState, String> {
        let mode = match value {
            "ask" => Mode::Ask,
            "yolo" => Mode::Yolo,
            _ => return Err("Use /mode ask or /mode yolo".into()),
        };
        self.modes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(room.into(), mode);
        Ok(self.state(room))
    }

    pub async fn gate(&self, channel: &ConversationChannel, event: ToolCallEvent) -> ToolUse {
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
                return ToolUse::Allow;
            }
            // The tool's schema defines an omitted dry_run as true.
            "patch"
                if event
                    .call
                    .arguments
                    .get("dry_run")
                    .is_none_or(|value| value == true) =>
            {
                return ToolUse::Allow;
            }
            "write" | "edit" | "patch" | "shell" | "process_start" | "process_write"
            | "web_fetch" | "web_search" => {}
            name if name.starts_with("mcp__") => {}
            _ => return ToolUse::block("This tool has no application permission policy."),
        }
        // Decide once for this call. Changing modes never auto-accepts an
        // already pending approval, or cancels an action already authorized.
        if self.mode(event.room_id.as_str()) == Mode::Yolo {
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
        let detail = format!("{label}:\n{arguments}");
        if detail.len() > 65_536 {
            return ToolUse::block(
                "The action is too large to review; split it into smaller calls.",
            );
        }
        match channel
            .ask_permission(
                &event.room_id,
                &event.channel_id,
                format!("Allow {}?", event.call.name),
                detail,
                Duration::from_secs(300),
            )
            .await
        {
            Decision::Allow => ToolUse::Allow,
            Decision::Deny => ToolUse::block(
                "The action was not approved (refused, expired or view disconnected). Do not retry it without a new user instruction.",
            ),
        }
    }
}
