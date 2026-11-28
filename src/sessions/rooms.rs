//! The project's sessions: creating one, listing those to resume, resuming
//! one, and `/new`, which keeps an empty draft rather than opening another.

mod creation;
pub(crate) use creation::*;

use roomkit::model::{
    Access, ChannelId, EventContent, EventType, Room, RoomContext, RoomId, RoomStatus,
};
use roomkit::store::{FindRoomIdsByChannelOptions, GetEventCountOptions};
use roomkit::{
    ConversationStore, CreateRoomOptions, EventFilter, GetRoomOptions, GetTimelineOptions,
    ListBindingsOptions, RoomFilter, RoomKit,
};
use roomkit_providers_sqlite::SqliteStore;
use roomkit_tui::protocol::{Choice, Event};
use roomkit_tui_engine::{CommandResult, SessionConfig};

use super::{Sessions, names, title};
use crate::{Result, config::valid_session_name};

/// Sessions `/resume` lists, newest first.
const LISTED: usize = 200;

impl Sessions {
    pub(super) async fn switched(&self, mut config: SessionConfig, room: RoomId) -> CommandResult {
        let name = names::name(&self.kit, &room).await;
        config.title = title(&self.workspace, &room, name.as_deref());
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
        let name = format!("session-{}", crate::digest::sha256_hex(identity));
        let draft_key = format!("roomkit_code:new-draft:{name}");
        if self.kept_draft(&current.room, &draft_key).await? {
            return Ok(self.switched(current.clone(), current.room).await);
        }
        if empty(&self.kit, &current.room)
            .await
            .map_err(|_| "Could not inspect the current session".to_string())?
        {
            // Remember a no-op too: a delayed retry after the first
            // message must not unexpectedly create another session.
            self.keep_draft(&current.room, &draft_key).await?;
            return Ok(self.switched(current.clone(), current.room).await);
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
        // A new conversation starts in the launch's mode, its agent with its
        // own model and effort, whatever the conversation it is opened from
        // was set. One a retried command finds again is as it was left.
        let mut next = current;
        next.room = room.clone();
        let kept = super::kept_mode(&self.kit, &room).await;
        self.policy.open(room.as_str(), kept);
        self.profiles.settle(&self.kit, &mut next).await;
        Ok(self.switched(next, room).await)
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
            .map_err(|error| match error {
                roomkit::RoomKitError::RoomNotFound(_) => {
                    "Session not found in this project".to_string()
                }
                // A store that is busy or does not read is not a missing
                // session: saying so sent people looking for one that exists.
                _ => "This project's history could not be read just now; try again".to_string(),
            })?;
        if found.status != RoomStatus::Active {
            return Err("This session is not active".into());
        }
        let mut next = current;
        next.room = room.clone();
        // The conversation's agent, not the one the view comes with: its
        // history must not reach an agent selected elsewhere.
        let agent = super::designated(&self.kit, &room)
            .await
            .map_err(|e| e.to_string())?;
        let agent = agent.unwrap_or_else(|| next.agent.clone());
        self.profiles
            .show(&self.kit, &mut next, &self.channel, agent)
            .await?;
        // And in the mode it was left in.
        let kept = super::kept_mode(&self.kit, &room).await;
        self.policy.open(room.as_str(), kept);
        self.services
            .sessions
            .keep(claim)
            .map_err(|_| "Cannot retain session ownership".to_string())?;
        self.services.hooks.note_source(room.as_str(), "resume");
        Ok(self.switched(next, room).await)
    }

    /// The other active sessions this view can read, newest first.
    async fn choices(&self, current: &SessionConfig) -> Result<Vec<Choice>> {
        let mut rooms = active_rooms(&self.kit, &self.channel).await?;
        rooms
            .retain(|room| room.id != current.room && room.organization_id == current.organization);
        rooms.sort_by(|a, b| {
            last_used(b)
                .cmp(&last_used(a))
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

    /// A session as `/resume` lists it, labelled by its name or else the
    /// viewer's first message; none when the view cannot read it or it is
    /// empty.
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
        let mut context = RoomContext::new(room);
        context.bindings = bindings;
        context.recent_events = rows;
        self.listed(&context, current).await.map(Some)
    }

    /// A readable session's entry. A named one keeps its first message in
    /// the description, where the picker's filter still finds it.
    async fn listed(&self, context: &RoomContext, current: &SessionConfig) -> Result<Choice> {
        let room = &context.room;
        let id = room.id.to_string();
        let first = self.label(context, current);
        let (label, first) = match names::name(&self.kit, &room.id).await {
            Some(name) => (name, format!("{first} · ")),
            None => (first, String::new()),
        };
        let occupied = if self.services.sessions.busy(&id)? {
            "Open in another instance · "
        } else {
            ""
        };
        let date = utc_minute(last_used(room).as_unix_millis());
        let count = room.event_count;
        let description = format!("{first}{occupied}{date} UTC · {count} events · {id}");
        Ok(Choice::new(id.clone(), label).with_description(description))
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

/// When `room` was last used: its latest message, or its latest change
/// when that came after, as a rename does. A message moves the room's
/// activity and leaves its own date where it was.
fn last_used(room: &Room) -> roomkit::model::Timestamp {
    room.timers
        .last_activity_at
        .map_or(room.updated_at, |active| active.max(room.updated_at))
}

/// `YYYY-MM-DD HH:MM` in UTC for a Unix time in milliseconds, computed
/// rather than cut from a text whose format the store owns.
fn utc_minute(millis: i64) -> String {
    let minutes = millis.div_euclid(60_000);
    let (days, minute) = (minutes.div_euclid(1440), minutes.rem_euclid(1440));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        minute / 60,
        minute % 60
    )
}

#[cfg(test)]
mod tests {
    use super::utc_minute;

    #[test]
    fn dates_are_computed_in_utc_to_the_minute() {
        assert_eq!(utc_minute(0), "1970-01-01 00:00");
        assert_eq!(utc_minute(1_761_951_959_000), "2025-10-31 23:05");
        assert_eq!(utc_minute(1_709_164_800_000), "2024-02-29 00:00");
        assert_eq!(utc_minute(951_782_400_000), "2000-02-29 00:00");
        assert_eq!(utc_minute(-60_000), "1969-12-31 23:59");
    }
}
