//! Project sessions and their commands belong to this application.

use std::path::PathBuf;

use roomkit::model::{Access, ChannelId, EventContent, RoomId};
use roomkit::{ListBindingsOptions, RoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{Choice, Command, CommandSpec, Event, Rejection, Request};
use roomkit_tui_engine::{CommandHandler, CommandResult, SessionConfig};

mod agent;
mod commands;
mod live;
pub(crate) mod names;
mod rooms;
mod settings;
pub(crate) mod task_rooms;
pub(crate) use commands::*;

pub(crate) use agent::{designate, designated};
pub(crate) use names::name;
pub(crate) use rooms::{active_rooms, create, empty};
pub(crate) use settings::{
    Running, follow_modes, forget, keep, keep_modes, kept, kept_mode, last_model,
};

/// The view's title. A named conversation leads with its name, so a narrow
/// header or Herdr's 80 characters cut the workspace path rather than the
/// name; otherwise the workspace comes first, then the start of the ID.
pub(crate) fn title(workspace: &std::path::Path, room: &RoomId, name: Option<&str>) -> String {
    let title = if let Some(name) = name {
        format!("{name} · {}", workspace.display())
    } else {
        let id = room.as_str();
        let label = if id.len() > 20 {
            format!("{}…", id.chars().take(16).collect::<String>())
        } else {
            id.into()
        };
        format!("{} · {label}", workspace.display())
    };
    fit_label(title)
}

/// `text` within the protocol's label size, which views ignore a longer
/// title past: cut at a character boundary, marked with an ellipsis.
fn fit_label(mut text: String) -> String {
    use roomkit_tui::protocol::limits::MAX_LABEL_BYTES;
    if text.len() <= MAX_LABEL_BYTES {
        return text;
    }
    text.truncate(text.floor_char_boundary(MAX_LABEL_BYTES - "…".len()));
    text.push('…');
    text
}

pub(crate) struct Sessions {
    pub kit: RoomKit<SqliteStore>,
    pub workspace: PathBuf,
    pub channel: roomkit::model::ChannelId,
    pub policy: crate::permissions::Policy,
    pub profiles: std::sync::Arc<crate::profiles::Profiles>,
    pub services: std::sync::Arc<crate::services::Services>,
}

impl CommandHandler for Sessions {
    async fn prepare_images(
        &self,
        text: &str,
        images: &[roomkit_tui::protocol::ImageUpload],
        current: &SessionConfig,
    ) -> std::result::Result<EventContent, Rejection> {
        let bindings = self
            .kit
            .list_bindings(
                &current.room,
                ListBindingsOptions {
                    organization_id: current.organization.clone(),
                },
            )
            .await
            .map_err(|_| Rejection::unavailable("Cannot authorize image attachments"))?;
        if !current.can_attach_images
            || !bindings
                .iter()
                .any(|b| b.channel_id == self.channel && b.access == Access::ReadWrite)
        {
            return Err(Rejection::refused("This attachment cannot submit images"));
        }
        self.profiles
            .images
            .prepare(&current.room, text, images)
            .await
            .map_err(Rejection::from)
    }

    fn subscribe(
        &self,
        current: SessionConfig,
    ) -> Option<tokio::sync::watch::Receiver<Vec<Event>>> {
        if current.group.is_none() && self.services.sessions.owns(current.room.as_str()) {
            self.profiles.resources.messages.listen(
                &current.room,
                &self.services.sessions,
                self.profiles.diagnostics.clone(),
            );
        }
        Some(live::subscribe(self, current))
    }

    fn stop_run(
        &self,
        current: &SessionConfig,
        _run: &roomkit_tui::protocol::RunId,
    ) -> Option<roomkit_tui_engine::StopRun> {
        let tasks = self.profiles.resources.tasks.clone();
        let task = tasks.running_in(current.room.as_str())?;
        let (kit, current) = (self.kit.clone(), current.clone());
        Some(Box::pin(async move {
            crate::delegation::panel::stop(&kit, &tasks, &current, task).await
        }))
    }

    fn allow_form_while_busy(&self, name: &str) -> bool {
        name == "message"
    }

    fn allow_while_busy(&self, name: &str) -> bool {
        matches!(
            name,
            "mode"
                | "message"
                | "rename"
                | "tools"
                | "downloads"
                | "processes"
                | "tasks"
                | "executors"
                | "back"
                | "files"
                | "diagnostics"
                | "plans"
                | "hooks"
                | "instructions"
                // Queued until the turn ends (HITL a2dfba34).
                | "compact"
        )
    }

    async fn handle(
        &self,
        request: Request,
        current: SessionConfig,
    ) -> std::result::Result<CommandResult, Rejection> {
        self.dispatch(request, current)
            .await
            .map_err(Rejection::from)
    }
}

impl Sessions {
    /// Route a command to the part of the application that owns it.
    #[expect(
        clippy::cognitive_complexity,
        reason = "Debt recorded when the limit was set: split it, never grow it"
    )]
    async fn dispatch(
        &self,
        request: Request,
        current: SessionConfig,
    ) -> std::result::Result<CommandResult, String> {
        let (name, form) = match &request.command {
            Command::CompleteInput { name } if name == "message" => {
                return crate::messages::command::suggest(self, &current, request.id).await;
            }
            Command::Invoke { name, .. } => (name.as_str(), false),
            Command::SubmitForm { name, .. } => (name.as_str(), true),
            _ => return Err("Unsupported application request".into()),
        };
        match name {
            "message" => return crate::messages::command::handle(self, current, request).await,
            "executors" => return self.profiles.executors(&self.kit, request, &current).await,
            "mcp" => {
                let result = self.profiles.mcp(&self.kit, request, current).await;
                self.services.sidebar_changes.send_replace(());
                return result;
            }
            "providers" | "agents" | "model" | "effort" => {
                return self
                    .profiles
                    .handle(&self.kit, &self.channel, request, current)
                    .await;
            }
            "shell" if !form => return self.shell(request, current).await,
            _ => {}
        }
        let Command::Invoke { name, arguments } = &request.command else {
            return Err("Unsupported application request".into());
        };
        let arguments = arguments.trim();
        match name.as_str() {
            "skills" => self.profiles.skills(&self.kit, current, arguments).await,
            "downloads" if arguments == "close" => Ok(CommandResult::Events(Vec::new())),
            "downloads" if arguments.is_empty() => Ok(CommandResult::Events(vec![Event::Panel {
                conversation: current.room.as_str().into(),
                panel: self.profiles.local_models.downloads.panel(),
            }])),
            "update" => self.services.updates.handle(&current, arguments),
            "instructions" => {
                self.profiles
                    .resources
                    .instructions
                    .panel(&current, arguments)
                    .await
            }
            "hooks" => self.services.hooks.panel(&current, arguments),
            "plans" => self.profiles.plans.handle(&current, arguments).await,
            "files" => {
                self.profiles
                    .resources
                    .files
                    .panel(&current, arguments)
                    .await
            }
            "diagnostics" => Ok(self.profiles.diagnostics.panel(&current, arguments)),
            "processes" => {
                self.profiles
                    .processes
                    .handle(&self.kit, &self.channel, &current, arguments)
                    .await
            }
            "tasks" => crate::delegation::panel::handle(self, &current, arguments).await,
            "back" => task_rooms::back(self, &current).await,
            "tools" => self.tools(&current, arguments).await,
            "mode" => self.mode(&current, arguments).await,
            "compact" => self.compact_here(&current),
            "new" => {
                let current = task_rooms::in_conversation(self, current).await?;
                self.new_session(current, arguments, request.operation.as_str())
                    .await
            }
            "resume" => {
                // The list shows in this view; a choice leaves its room.
                let current = if arguments.is_empty() {
                    current
                } else {
                    task_rooms::in_conversation(self, current).await?
                };
                self.resume(current, arguments).await
            }
            "rename" => self.rename(current, arguments).await,
            _ => Err("Unknown application command".into()),
        }
    }

    /// Compacts the conversation whatever its tier: at once, or once the
    /// turn under way ends (HITL a2dfba34); an agent without a known window
    /// keeps its latest messages.
    fn compact(&self, current: &SessionConfig) -> CommandResult {
        use crate::compaction::Requested;
        let compactions = &self.profiles.resources.compactions;
        let text = match compactions.request(current.room.as_str(), current.agent.as_str()) {
            Requested::Started => "Compacting the conversation…",
            Requested::Queued => "Compaction queued: it runs when the agent's turn ends",
            Requested::Running => "A compaction is already running in this conversation",
            Requested::Unbounded => {
                "This agent reads its latest 50 messages: set its context capacity in /agents for it to compact"
            }
        };
        CommandResult::Events(vec![Event::Notice {
            conversation: current.room.as_str().into(),
            text: text.into(),
        }])
    }

    /// The user's own `!` command, refused in Plan.
    async fn shell(
        &self,
        request: Request,
        current: SessionConfig,
    ) -> std::result::Result<CommandResult, String> {
        if self.policy.mode(current.room.as_str()) == crate::permissions::Mode::Plan {
            return Err("Shell commands are disabled in Plan mode. Switch to Ask or Yolo to execute a command.".into());
        }
        self.services
            .shell
            .prepare(&self.kit, &self.channel, &current, request)
            .await
    }

    /// `/tools`: the agent's registered tools, or one of them; nothing runs.
    async fn tools(
        &self,
        current: &SessionConfig,
        arguments: &str,
    ) -> std::result::Result<CommandResult, String> {
        let tools = self.profiles.tools(current.agent.as_str()).await;
        if arguments.is_empty() {
            return Ok(CommandResult::Events(vec![Event::Choices {
                conversation: current.room.as_str().into(),
                command: "tools".into(),
                title: format!(
                    "Tools available to {} · {} registered",
                    current.agent,
                    tools.len()
                ),
                choices: tools
                    .iter()
                    .map(|tool| {
                        Choice::new(tool.name.clone(), tool.name.clone())
                            .with_description(tool.description.clone())
                    })
                    .collect(),
            }]));
        }
        let tool = tools
            .iter()
            .find(|tool| tool.name == arguments)
            .ok_or_else(|| {
                "Unknown tool; use /tools to inspect registered capabilities".to_string()
            })?;
        let mode = self.policy.state(current.room.as_str()).selected;
        Ok(CommandResult::Events(vec![Event::Notice {
            conversation: current.room.as_str().into(),
            text: format!(
                "{} is registered · mode {mode} · inspection only; no tool executed",
                tool.name
            ),
        }]))
    }

    /// `/mode`: the modes to choose from, or the chosen one.
    async fn mode(
        &self,
        current: &SessionConfig,
        arguments: &str,
    ) -> std::result::Result<CommandResult, String> {
        let room = current.room.as_str();
        if arguments.is_empty() {
            return Ok(CommandResult::Events(vec![Event::Choices {
                conversation: room.into(),
                command: "mode".into(),
                title: "Permission mode".into(),
                choices: self.policy.state(room).choices,
            }]));
        }
        let before = self.policy.mode(room);
        let mode = self.policy.set(room, arguments)?;
        if self.policy.mode(room) != before {
            self.tell_the_running_turn(current).await;
        }
        Ok(CommandResult::Events(vec![Event::ModeChanged {
            conversation: room.into(),
            mode,
        }]))
    }

    /// A turn reads its mode's instruction as it starts: a turn under way
    /// reads the new one at its next step, or it would go on working to the
    /// old one while the permissions follow the new one. Without a turn
    /// under way nothing is reached, and the next turn starts on the new one.
    /// A turn still preparing its loop is not reached either, and keeps its
    /// instruction; the permissions still follow the mode. The system prompt
    /// only grows, and comes first in the request: the text says which
    /// instruction holds wherever it reads, and leaves the last word to a
    /// mode tool the model calls afterwards.
    async fn tell_the_running_turn(&self, current: &SessionConfig) {
        self.steer_mode(current, current.room.clone(), &current.agent)
            .await;
        // From a task's room, the mode is the whole conversation's.
        let tasks = &self.profiles.resources.tasks;
        let root = tasks.root_of(current.room.as_str());
        if root == current.room.as_str() {
            return;
        }
        let lead = tasks
            .lead_of(&root)
            .and_then(|agent| ChannelId::new(agent).ok());
        if let (Ok(room), Some(lead)) = (RoomId::new(&root), lead) {
            self.steer_mode(current, room, &lead).await;
        }
    }

    /// Tells the turn `agent` runs in `room` the new mode.
    async fn steer_mode(&self, current: &SessionConfig, room: RoomId, agent: &ChannelId) {
        let update = roomkit::model::UpdateSystemPrompt::new(format!(
            "\n\nThe user changed the application mode during this turn. The instruction below replaces the mode instruction elsewhere in this conversation, wherever it appears, and any earlier notice like this one: the last notice counts, unless a plan mode tool you call after reading this reports another mode.\n{}",
            self.policy.instruction(room.as_str())
        ));
        let options = roomkit::SteerOptions {
            organization_id: current.organization.clone(),
            ..Default::default()
        };
        if let Err(error) = self.kit.steer(&room, agent, update, options).await {
            let error = error.to_string();
            let fields = serde_json::json!({ "room": room.as_str() });
            self.profiles
                .diagnostics
                .record("mode_change_not_steered", fields, Some(&error));
        }
    }

    /// `/compact`, refused in the room of a task its agent is working on: the
    /// queue that holds a compaction until a turn ends counts a person's
    /// turns, not a delegated one.
    fn compact_here(&self, current: &SessionConfig) -> std::result::Result<CommandResult, String> {
        if self
            .profiles
            .resources
            .tasks
            .running_in(current.room.as_str())
            .is_some()
        {
            return Err("This task's agent is working: compact once its task ended".into());
        }
        Ok(self.compact(current))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A fixed fixture ID is valid")]
    use super::title;
    use roomkit::model::RoomId;
    use roomkit_tui::protocol::limits::MAX_LABEL_BYTES;

    #[test]
    fn a_title_fits_the_label_limit_and_keeps_the_name_first() {
        let room = RoomId::new("session-1").unwrap();
        let workspace = std::path::PathBuf::from(format!("/{}", "é".repeat(200)));
        let named = title(&workspace, &room, Some("Release notes"));
        assert!(named.len() <= MAX_LABEL_BYTES, "{}", named.len());
        assert!(named.starts_with("Release notes · /é") && named.ends_with('…'));
        let short = title(std::path::Path::new("/p"), &room, Some("Notes"));
        assert_eq!(short, "Notes · /p");
    }
}
