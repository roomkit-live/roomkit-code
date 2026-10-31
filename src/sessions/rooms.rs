//! The project's sessions: creating one, listing those to resume, resuming
//! one, and `/new`, which keeps an empty draft rather than opening another.

use roomkit::model::{Access, EventContent, EventType, Room, RoomContext, RoomId, RoomStatus};
use roomkit::{
    ConversationStore, CreateRoomOptions, EventFilter, GetRoomOptions, GetTimelineOptions,
    ListBindingsOptions, RoomFilter, RoomKit,
};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{Choice, Event};
use roomkit_tui_engine::{CommandResult, SessionConfig};
use sha2::{Digest, Sha256};

use super::{Sessions, title};
use crate::{Result, config::valid_session_name};

/// Sessions `/resume` lists, newest first.
const LISTED: usize = 200;

const DRAFT_MARKER: &str = "roomkit_code_draft";

// Bindings are infrastructure, not conversation history. Even blocked messages
// prevent reuse: an attempted conversation must never become another draft.
pub(crate) async fn empty(kit: &RoomKit<SqliteStore>, room: &RoomId) -> Result<bool> {
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

pub(crate) async fn create(
    kit: &RoomKit<SqliteStore>,
    leases: &crate::storage::SessionLeases,
    channel: &roomkit::model::ChannelId,
    agent: &roomkit::model::ChannelId,
    name: Option<&str>,
    draft: bool,
) -> Result<RoomId> {
    if name.is_none()
        && draft
        && let Some(id) = unused_draft(kit, leases, channel, agent).await?
    {
        return Ok(id);
    }
    let id = RoomId::new(name.map_or_else(
        || format!("session-{}", uuid::Uuid::new_v4()),
        str::to_owned,
    ))?;
    let claim = leases
        .claim(id.as_str())?
        .ok_or(crate::storage::SESSION_BUSY)?;
    match kit.get_room(&id, GetRoomOptions::default()).await {
        Ok(_) => {
            leases.keep(claim)?;
            return Ok(id);
        }
        Err(roomkit::RoomKitError::RoomNotFound(_)) => {}
        Err(error) => return Err(error.into()),
    }
    let room = kit
        .create_room(CreateRoomOptions {
            room_id: Some(id),
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
    leases.keep(claim)?;
    Ok(room.id)
}

/// The newest empty draft both channels can still write to, claimed. The
/// claim comes before the inspection: another process may own an apparently
/// empty room or still be initializing its bindings.
async fn unused_draft(
    kit: &RoomKit<SqliteStore>,
    leases: &crate::storage::SessionLeases,
    channel: &roomkit::model::ChannelId,
    agent: &roomkit::model::ChannelId,
) -> Result<Option<RoomId>> {
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
        {
            continue;
        }
        let Some(claim) = leases.claim(room.id.as_str())? else {
            continue;
        };
        if !empty(kit, &room.id).await? {
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
            leases.keep(claim)?;
            return Ok(Some(room.id));
        }
    }
    Ok(None)
}

impl Sessions {
    fn switched(&self, mut config: SessionConfig, room: RoomId) -> CommandResult {
        config.title = title(&self.workspace, &room);
        config.mode = Some(self.policy.state(room.as_str()));
        config.room = room;
        CommandResult::Switch(Box::new(config))
    }

    /// `/new`: a new session, unless this one is still an empty draft. A
    /// retried operation from the same session reuses its destination, also
    /// after the application restarts.
    pub(super) async fn new_session(
        &self,
        current: SessionConfig,
        arguments: &str,
        operation: &str,
    ) -> std::result::Result<CommandResult, String> {
        if !arguments.is_empty() {
            return Err("Use /new without arguments".into());
        }
        let identity = serde_json::to_vec(&(current.room.as_str(), operation))
            .map_err(|_| "Invalid operation identity".to_string())?;
        let name = format!("session-{:x}", Sha256::digest(identity));
        let draft_key = format!("roomkit_code:new-draft:{name}");
        if self.kept_draft(&current.room, &draft_key).await? {
            return Ok(self.switched(current.clone(), current.room));
        }
        if empty(&self.kit, &current.room)
            .await
            .map_err(|_| "Could not inspect the current session".to_string())?
        {
            // Remember a no-op too: a delayed retry after the first
            // message must not unexpectedly create another session.
            self.keep_draft(&current.room, &draft_key).await?;
            return Ok(self.switched(current.clone(), current.room));
        }
        let room = create(
            &self.kit,
            &self.services.sessions,
            &self.channel,
            &current.agent,
            Some(&name[..48]),
            true,
        )
        .await
        .map_err(|error| error.to_string())?;
        self.services.hooks.note_source(room.as_str(), "new");
        Ok(self.switched(current, room))
    }

    /// Whether an earlier `/new` from this session kept it as the draft.
    async fn kept_draft(&self, room: &RoomId, key: &str) -> std::result::Result<bool, String> {
        let kept = self
            .kit
            .store()
            .get_private_state(room, key)
            .await
            .map_err(|_| "Could not inspect the previous command".to_string())?;
        Ok(kept.is_some_and(|state| state.get("kept") == Some(&serde_json::Value::Bool(true))))
    }

    async fn keep_draft(&self, room: &RoomId, key: &str) -> std::result::Result<(), String> {
        let kept = [("kept".into(), serde_json::Value::Bool(true))]
            .into_iter()
            .collect();
        self.kit
            .store()
            .set_private_state(room, key, Some(kept))
            .await
            .map_err(|_| "Could not retain the current draft".to_string())
    }

    /// `/resume`: the sessions to choose from, or the chosen one.
    pub(super) async fn resume(
        &self,
        current: SessionConfig,
        arguments: &str,
    ) -> std::result::Result<CommandResult, String> {
        if arguments.is_empty() {
            return self.resume_choices(&current).await;
        }
        if !valid_session_name(arguments) {
            return Err("Invalid session id; use /resume to choose from the list".into());
        }
        let room = RoomId::new(arguments).map_err(|_| "Invalid session id".to_string())?;
        self.resume_session(current, room).await
    }

    async fn resume_choices(
        &self,
        current: &SessionConfig,
    ) -> std::result::Result<CommandResult, String> {
        let choices = self
            .choices(current)
            .await
            .map_err(|_| "Could not list this project's sessions".to_string())?;
        let title = if choices.len() == LISTED {
            format!("Resume session · latest {LISTED}")
        } else {
            "Resume session".into()
        };
        Ok(CommandResult::Events(vec![Event::Choices {
            conversation: current.room.as_str().into(),
            command: "resume".into(),
            title,
            choices,
        }]))
    }

    /// Open `room`, never creating it, once this process owns it.
    async fn resume_session(
        &self,
        current: SessionConfig,
        room: RoomId,
    ) -> std::result::Result<CommandResult, String> {
        let claim = self
            .services
            .sessions
            .claim(room.as_str())
            .map_err(|_| "Cannot check session ownership".to_string())?
            .ok_or_else(|| crate::storage::SESSION_BUSY.to_string())?;
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
        self.services
            .sessions
            .keep(claim)
            .map_err(|_| "Cannot retain session ownership".to_string())?;
        self.services.hooks.note_source(room.as_str(), "resume");
        Ok(self.switched(next, room))
    }

    /// The other active sessions this view can read, newest first.
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
            if choices.len() == LISTED {
                break;
            }
            if let Some(choice) = self.choice(room, current).await? {
                choices.push(choice);
            }
        }
        Ok(choices)
    }

    /// A session as `/resume` lists it, labelled by the viewer's first
    /// message; none when the view cannot read it or it is empty.
    async fn choice(&self, room: Room, current: &SessionConfig) -> Result<Option<Choice>> {
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
            return Ok(None);
        }
        if empty(&self.kit, &room.id).await? {
            return Ok(None);
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
        let label = self.label(&context, current);
        let occupied = if self.services.sessions.busy(&id)? {
            "Open in another instance · "
        } else {
            ""
        };
        Ok(Some(Choice::new(id.clone(), label).with_description(
            format!("{occupied}{date} UTC · {count} events · {id}"),
        )))
    }

    /// The start of the viewer's first visible message, on one line.
    fn label(&self, context: &RoomContext, current: &SessionConfig) -> String {
        roomkit::visible_events(context, &self.channel)
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
                    return Some(text.chars().take(100).collect::<String>());
                }
                None
            })
            .unwrap_or_else(|| "Conversation".into())
    }
}
