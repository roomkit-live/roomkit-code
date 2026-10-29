//! Project sessions and their commands belong to this application.

use std::path::PathBuf;

use roomkit::model::{Access, EventContent, RoomContext, RoomId, RoomStatus};
use roomkit::{
    ConversationStore, CreateRoomOptions, GetRoomOptions, GetTimelineOptions, ListBindingsOptions,
    RoomFilter, RoomKit,
};
use roomkit_cli_roomkit::{CommandHandler, CommandResult, SessionConfig};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{Choice, Command, CommandSpec, Event, Request};
use sha2::{Digest, Sha256};

use crate::{Result, config::valid_session_name};

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            name: "mode".into(),
            description: "Change permissions (also Shift+Tab)".into(),
            arguments: "[ask|yolo]".into(),
            choices: vec!["ask".into(), "yolo".into()],
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
) -> Result<RoomId> {
    let id = name.map(RoomId::new).transpose()?;
    if let Some(id) = &id {
        match kit.get_room(id, GetRoomOptions::default()).await {
            Ok(_) => return Ok(id.clone()),
            Err(roomkit::KitError::Store(roomkit::StoreError::RoomNotFound(_))) => {}
            Err(error) => return Err(error.into()),
        }
    }
    let room = kit
        .create_room(CreateRoomOptions {
            room_id: id,
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
        for room in rooms.into_iter().take(200) {
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
            }) || !bindings
                .iter()
                .any(|binding| binding.channel_id == current.agent)
            {
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
                .unwrap_or_else(|| "Empty conversation".into());
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
    fn allow_while_busy(&self, name: &str) -> bool {
        name == "mode"
    }

    async fn handle(
        &self,
        request: Request,
        current: SessionConfig,
    ) -> std::result::Result<CommandResult, String> {
        let Command::Invoke { name, arguments } = request.command else {
            return Err("Unsupported application request".into());
        };
        let arguments = arguments.trim();
        match name.as_str() {
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
                let room = create(&self.kit, &self.channel, &current.agent, Some(&name[..48]))
                    .await
                    .map_err(|_| "Could not create a new session".to_string())?;
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
                Ok(self.switched(current, room))
            }
            "resume" => Err("Invalid session id; use /resume to choose from the list".into()),
            _ => Err("Unknown application command".into()),
        }
    }
}
