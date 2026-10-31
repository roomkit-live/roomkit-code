//! Project sessions and their commands belong to this application.

use std::path::PathBuf;

use roomkit::model::{Access, EventContent, EventType, RoomContext, RoomId, RoomStatus};
use roomkit::{
    ConversationStore, CreateRoomOptions, EventFilter, GetRoomOptions, GetTimelineOptions,
    ListBindingsOptions, RoomFilter, RoomKit,
};
use roomkit_cli_roomkit::{CommandHandler, CommandResult, SessionConfig};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{Choice, Command, CommandSpec, Event, Request};
use sha2::{Digest, Sha256};

use crate::{Result, config::valid_session_name};

const DRAFT_MARKER: &str = "roomkit_code_draft";

// Bindings are infrastructure, not conversation history. Even blocked messages
// prevent reuse: an attempted conversation must never become another draft.
async fn empty(kit: &RoomKit<SqliteStore>, room: &RoomId) -> Result<bool> {
    Ok(kit
        .store()
        .get_event_count(
            room,
            Some(EventFilter {
                exclude_types: Some(vec![EventType::ChannelAttached, EventType::ChannelDetached]),
                include_blocked: true,
                ..Default::default()
            }),
        )
        .await?
        == 0)
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            name: "instructions".into(),
            description: "Inspect applicable AGENTS.md files and their scope".into(),
            arguments: "[project-relative path]".into(),
            choices: Vec::new(),
        },
        CommandSpec {
            name: "hooks".into(),
            description: "Inspect, enable and diagnose configured command hooks".into(),
            arguments: String::new(),
            choices: Vec::new(),
        },
        CommandSpec {
            name: "plans".into(),
            description: "Follow this agent's plans and inspect their steps".into(),
            arguments: "[plan-id]".into(),
            choices: Vec::new(),
        },
        CommandSpec {
            name: "mcp".into(),
            description: "Configure MCP servers and choose tools for this agent".into(),
            arguments: "[list|add|connect <id>|disconnect <id>|enable <id>|disable <id>]".into(),
            choices: Vec::new(),
        },
        CommandSpec {
            name: "skills".into(),
            description: "Inspect and enable skills for this agent, or request their use".into(),
            arguments: "[list|reload|enable <name>|disable <name>|use <name> <task>]".into(),
            choices: Vec::new(),
        },
        CommandSpec {
            name: "shell".into(),
            description: "Run a shell command without the model (also !command)".into(),
            arguments: "<command>".into(),
            choices: Vec::new(),
        },
        CommandSpec {
            name: "effort".into(),
            description: "Choose reasoning effort for the current agent".into(),
            arguments: "[default|low|medium|high|xhigh|max]".into(),
            choices: Vec::new(),
        },
        CommandSpec {
            name: "diagnostics".into(),
            description: "Inspect recent engine errors and local audit logs".into(),
            arguments: String::new(),
            choices: Vec::new(),
        },
        CommandSpec {
            name: "processes".into(),
            description: "Inspect background processes and stop them (also F6)".into(),
            arguments: "[process-id]".into(),
            choices: Vec::new(),
        },
        CommandSpec {
            name: "providers".into(),
            description: "List, configure or delete shared provider connections".into(),
            arguments: "[list|delete <id>]".into(),
            choices: Vec::new(),
        },
        CommandSpec {
            name: "agents".into(),
            description: "List, select, configure or delete project agents".into(),
            arguments: "[list|delete <id>]".into(),
            choices: Vec::new(),
        },
        CommandSpec {
            name: "tools".into(),
            description: "Inspect tools actually registered by the application".into(),
            arguments: "[tool]".into(),
            choices: Vec::new(),
        },
        CommandSpec {
            name: "mode".into(),
            description: "Change permissions (also Shift+Tab)".into(),
            arguments: "[ask|plan|yolo]".into(),
            choices: vec!["ask".into(), "plan".into(), "yolo".into()],
        },
        CommandSpec {
            name: "new".into(),
            description: "Start a new conversation in this project".into(),
            arguments: String::new(),
            choices: Vec::new(),
        },
        CommandSpec {
            name: "resume".into(),
            description: "Choose a previous project session".into(),
            arguments: "[session-id]".into(),
            choices: Vec::new(),
        },
    ]
}

pub(crate) fn title(workspace: &std::path::Path, room: &RoomId) -> String {
    let id = room.as_str();
    let label = if id.len() > 20 {
        format!("{}…", id.chars().take(16).collect::<String>())
    } else {
        id.into()
    };
    format!("{} · {label}", workspace.display())
}

pub(crate) async fn create(
    kit: &RoomKit<SqliteStore>,
    channel: &roomkit::model::ChannelId,
    agent: &roomkit::model::ChannelId,
    name: Option<&str>,
    draft: bool,
) -> Result<RoomId> {
    let id = name.map(RoomId::new).transpose()?;
    if let Some(id) = &id {
        match kit.get_room(id, GetRoomOptions::default()).await {
            Ok(_) => return Ok(id.clone()),
            Err(roomkit::RoomKitError::RoomNotFound(_)) => {}
            Err(error) => return Err(error.into()),
        }
    } else if draft {
        // The application holds an exclusive lease for this project's store.
        // Reuse only app-owned, unused drafts; named sessions stay untouched.
        let mut rooms = kit
            .store()
            .list_rooms(&RoomFilter {
                channel_id: Some(channel.clone()),
                status: Some(RoomStatus::Active),
                ..Default::default()
            })
            .await?;
        rooms.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        for room in rooms {
            if room.organization_id.is_some()
                || room
                    .metadata
                    .get(DRAFT_MARKER)
                    .and_then(serde_json::Value::as_bool)
                    != Some(true)
                || !empty(kit, &room.id).await?
            {
                continue;
            }
            let bindings = kit
                .list_bindings(&room.id, ListBindingsOptions::default())
                .await?;
            if bindings
                .iter()
                .any(|b| b.channel_id == *channel && b.access == Access::ReadWrite)
                && bindings
                    .iter()
                    .any(|b| b.channel_id == *agent && b.access == Access::ReadWrite)
            {
                return Ok(room.id);
            }
        }
    }
    let room = kit
        .create_room(CreateRoomOptions {
            room_id: id,
            metadata: if draft {
                [(DRAFT_MARKER.into(), serde_json::Value::Bool(true))]
                    .into_iter()
                    .collect()
            } else {
                Default::default()
            },
            ..Default::default()
        })
        .await?;
    for id in [channel, agent] {
        kit.attach_channel(&room.id, id, Default::default()).await?;
    }
    Ok(room.id)
}

pub(crate) struct Sessions {
    pub kit: RoomKit<SqliteStore>,
    pub workspace: PathBuf,
    pub channel: roomkit::model::ChannelId,
    pub policy: crate::permissions::Policy,
    pub profiles: std::sync::Arc<crate::profiles::Profiles>,
}

impl Sessions {
    fn switched(&self, mut config: SessionConfig, room: RoomId) -> CommandResult {
        config.title = title(&self.workspace, &room);
        config.mode = Some(self.policy.state(room.as_str()));
        config.room = room;
        CommandResult::Switch(Box::new(config))
    }

    async fn choices(&self, current: &SessionConfig) -> Result<Vec<Choice>> {
        let mut rooms = self
            .kit
            .store()
            .list_rooms(&RoomFilter {
                channel_id: Some(self.channel.clone()),
                status: Some(RoomStatus::Active),
                ..Default::default()
            })
            .await?;
        rooms
            .retain(|room| room.id != current.room && room.organization_id == current.organization);
        rooms.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut choices = Vec::new();
        for room in rooms {
            if choices.len() == 200 {
                break;
            }
            let bindings = self
                .kit
                .list_bindings(
                    &room.id,
                    ListBindingsOptions {
                        organization_id: current.organization.clone(),
                    },
                )
                .await?;
            if !bindings.iter().any(|binding| {
                binding.channel_id == self.channel
                    && matches!(binding.access, Access::ReadOnly | Access::ReadWrite)
            }) {
                continue;
            }
            if empty(&self.kit, &room.id).await? {
                continue;
            }
            let rows = self
                .kit
                .get_timeline(
                    &room.id,
                    GetTimelineOptions {
                        limit: 32,
                        organization_id: current.organization.clone(),
                        ..Default::default()
                    },
                )
                .await?;
            let date = room
                .updated_at
                .to_string()
                .chars()
                .take(16)
                .collect::<String>()
                .replace('T', " ");
            let id = room.id.to_string();
            let count = room.event_count;
            let mut context = RoomContext::new(room);
            context.bindings = bindings;
            context.recent_events = rows;
            let label = roomkit::visible_events(&context, &self.channel)
                .iter()
                .find_map(|event| {
                    if event
                        .source
                        .participant_id
                        .as_ref()
                        .is_some_and(|id| id.as_str() == current.sender)
                        && let EventContent::Text(text) = &event.content
                    {
                        let text = text.body.split_whitespace().collect::<Vec<_>>().join(" ");
                        return Some(text.chars().take(100).collect());
                    }
                    None
                })
                .unwrap_or_else(|| "Conversation".into());
            choices.push(Choice {
                value: id.clone(),
                label,
                description: format!("{date} UTC · {count} events · {id}"),
            });
        }
        Ok(choices)
    }
}

impl CommandHandler for Sessions {
    async fn prepare_images(
        &self,
        text: &str,
        images: &[roomkit_tui::protocol::ImageUpload],
        current: &SessionConfig,
    ) -> std::result::Result<EventContent, String> {
        let bindings = self
            .kit
            .list_bindings(
                &current.room,
                ListBindingsOptions {
                    organization_id: current.organization.clone(),
                },
            )
            .await
            .map_err(|_| "Cannot authorize image attachments")?;
        if !current.can_attach_images
            || !bindings
                .iter()
                .any(|b| b.channel_id == self.channel && b.access == Access::ReadWrite)
        {
            return Err("This attachment cannot submit images".into());
        }
        self.profiles
            .images
            .prepare(&current.room, text, images)
            .await
    }

    fn subscribe(
        &self,
        current: SessionConfig,
    ) -> Option<tokio::sync::watch::Receiver<Vec<Event>>> {
        let mut processes = self.profiles.processes.subscribe(
            self.kit.clone(),
            self.channel.clone(),
            current.clone(),
        );
        let mut accounts = self.profiles.accounts.subscribe();
        let profiles = self.profiles.clone();
        let mut sidebar_changes = profiles.sidebar_changes.subscribe();
        let mut plan_changes = profiles.resources.plans.subscribe();
        let mut hook_changes = profiles.hooks.changes.subscribe();
        let (tx, rx) = tokio::sync::watch::channel(Vec::new());
        tokio::spawn(async move {
            loop {
                let mut events = processes.borrow_and_update().clone();
                events.extend(profiles.plans.updates(&current).await);
                events.extend(profiles.hooks.updates(&current));
                if let Some(event) = accounts.borrow_and_update().clone() {
                    // Panels carry their conversation and opaque instance ID;
                    // the UI ignores updates to a dismissed or different view.
                    events.push(event);
                }
                let (mut sidebar, poll_connections) = profiles.sidebar(&current).await;
                if let Some(section) = profiles.hooks.sidebar(&current) {
                    sidebar.sections.push(section);
                }
                events.push(Event::SidebarChanged {
                    conversation: current.room.as_str().into(),
                    sidebar: Some(sidebar),
                });
                let changed = *tx.borrow() != events;
                if changed && tx.send(events).is_err() {
                    break;
                }
                tokio::select! {
                    () = tx.closed() => break,
                    changed = processes.changed() => if changed.is_err() { break; },
                    changed = accounts.changed() => if changed.is_err() { break; },
                    changed = sidebar_changes.changed() => if changed.is_err() { break; },
                    changed = plan_changes.changed() => if changed.is_err() { break; },
                    changed = hook_changes.changed() => if changed.is_err() { break; },
                    // MCP exposes connection liveness, but no disconnect stream.
                    // Only inspect configured connections; unchanged state never redraws the TUI.
                    () = tokio::time::sleep(std::time::Duration::from_secs(2)), if poll_connections => {},
                }
            }
        });
        Some(rx)
    }

    fn allow_while_busy(&self, name: &str) -> bool {
        matches!(
            name,
            "mode" | "tools" | "processes" | "diagnostics" | "plans" | "hooks" | "instructions"
        )
    }

    async fn handle(
        &self,
        request: Request,
        current: SessionConfig,
    ) -> std::result::Result<CommandResult, String> {
        if matches!(&request.command, Command::Invoke { name, .. } | Command::SubmitForm { name, .. } if name == "mcp")
        {
            let result = self.profiles.mcp(&self.kit, request, current).await;
            self.profiles.sidebar_changes.send_replace(());
            return result;
        }
        if let Command::Invoke { name, arguments } = &request.command
            && name == "skills"
        {
            return self.profiles.skills(&self.kit, current, arguments).await;
        }
        if matches!(&request.command, Command::Invoke { name, .. } if name == "shell") {
            if self.policy.mode(current.room.as_str()) == crate::permissions::Mode::Plan {
                return Err("Shell commands are disabled in Plan mode. Switch to Ask or Yolo to execute a command.".into());
            }
            return self
                .profiles
                .shell
                .prepare(&self.kit, &self.channel, &current, request)
                .await;
        }
        if matches!(&request.command, Command::Invoke { name, .. } | Command::SubmitForm { name, .. } if matches!(name.as_str(), "providers" | "agents" | "effort"))
        {
            return self
                .profiles
                .handle(&self.kit, &self.channel, request, current)
                .await;
        }
        let tools = self.profiles.tools(current.agent.as_str()).await;
        let Command::Invoke { name, arguments } = request.command else {
            return Err("Unsupported application request".into());
        };
        let arguments = arguments.trim();
        match name.as_str() {
            "instructions" => {
                self.profiles
                    .resources
                    .instructions
                    .panel(&current, arguments)
                    .await
            }
            "hooks" => self.profiles.hooks.panel(&current, arguments),
            "plans" => self.profiles.plans.handle(&current, arguments).await,
            "diagnostics" => Ok(self.profiles.diagnostics.panel(&current, arguments)),
            "processes" => {
                self.profiles
                    .processes
                    .handle(&self.kit, &self.channel, &current, arguments)
                    .await
            }
            "tools" if arguments.is_empty() => Ok(CommandResult::Events(vec![Event::Choices {
                conversation: current.room.as_str().into(),
                command: "tools".into(),
                title: format!(
                    "Tools available to {} · {} registered",
                    current.agent,
                    tools.len()
                ),
                choices: tools
                    .iter()
                    .map(|tool| Choice {
                        value: tool.name.clone(),
                        label: tool.name.clone(),
                        description: tool.description.clone(),
                    })
                    .collect(),
            }])),
            "tools" => {
                let tool = tools
                    .iter()
                    .find(|tool| tool.name == arguments)
                    .ok_or_else(|| {
                        "Unknown tool; use /tools to inspect registered capabilities".to_string()
                    })?;
                let mode = self.policy.state(current.room.as_str()).selected;
                Ok(CommandResult::Events(vec![Event::Notice {
                    text: format!(
                        "{} is registered · mode {mode} · inspection only; no tool executed",
                        tool.name
                    ),
                }]))
            }

            "mode" if arguments.is_empty() => Ok(CommandResult::Events(vec![Event::Choices {
                conversation: current.room.as_str().into(),
                command: "mode".into(),
                title: "Permission mode".into(),
                choices: self.policy.state(current.room.as_str()).choices,
            }])),
            "mode" => {
                let mode = self.policy.set(current.room.as_str(), arguments)?;
                Ok(CommandResult::Events(vec![Event::ModeChanged {
                    conversation: current.room.as_str().into(),
                    mode,
                }]))
            }
            "new" if arguments.is_empty() => {
                // A retried operation from the same source session reuses its
                // destination, including after the application restarts.
                let identity = serde_json::to_vec(&(current.room.as_str(), request.operation.0))
                    .map_err(|_| "Invalid operation identity".to_string())?;
                let name = format!("session-{:x}", Sha256::digest(identity));
                let draft_key = format!("roomkit_code:new-draft:{name}");
                let kept_draft = self
                    .kit
                    .store()
                    .get_private_state(&current.room, &draft_key)
                    .await
                    .map_err(|_| "Could not inspect the previous command".to_string())?;
                if kept_draft
                    .is_some_and(|state| state.get("kept") == Some(&serde_json::Value::Bool(true)))
                {
                    return Ok(self.switched(current.clone(), current.room));
                }
                if empty(&self.kit, &current.room)
                    .await
                    .map_err(|_| "Could not inspect the current session".to_string())?
                {
                    // Remember a no-op too: a delayed retry after the first
                    // message must not unexpectedly create another session.
                    self.kit
                        .store()
                        .set_private_state(
                            &current.room,
                            &draft_key,
                            Some(
                                [("kept".into(), serde_json::Value::Bool(true))]
                                    .into_iter()
                                    .collect(),
                            ),
                        )
                        .await
                        .map_err(|_| "Could not retain the current draft".to_string())?;
                    return Ok(self.switched(current.clone(), current.room));
                }
                let room = create(
                    &self.kit,
                    &self.channel,
                    &current.agent,
                    Some(&name[..48]),
                    true,
                )
                .await
                .map_err(|_| "Could not create a new session".to_string())?;
                self.profiles.hooks.note_source(room.as_str(), "new");
                Ok(self.switched(current, room))
            }
            "new" => Err("Use /new without arguments".into()),
            "resume" if arguments.is_empty() => {
                let choices = self
                    .choices(&current)
                    .await
                    .map_err(|_| "Could not list this project's sessions".to_string())?;
                Ok(CommandResult::Events(vec![Event::Choices {
                    conversation: current.room.as_str().into(),
                    command: "resume".into(),
                    title: if choices.len() == 200 {
                        "Resume session · latest 200"
                    } else {
                        "Resume session"
                    }
                    .into(),
                    choices,
                }]))
            }
            "resume" if valid_session_name(arguments) => {
                let room = RoomId::new(arguments).map_err(|_| "Invalid session id".to_string())?;
                // Never create on resume. The adapter rechecks read access and
                // the selected agent before committing the replacement view.
                let found = self
                    .kit
                    .get_room(
                        &room,
                        GetRoomOptions {
                            organization_id: current.organization.clone(),
                        },
                    )
                    .await
                    .map_err(|_| "Session not found in this project".to_string())?;
                if found.status != RoomStatus::Active {
                    return Err("This session is not active".into());
                }
                let mut next = current;
                next.room = room.clone();
                self.profiles
                    .attach(&self.kit, &next, &self.channel)
                    .await?;
                self.profiles.hooks.note_source(room.as_str(), "resume");
                Ok(self.switched(next, room))
            }
            "resume" => Err("Invalid session id; use /resume to choose from the list".into()),
            _ => Err("Unknown application command".into()),
        }
    }
}
