//! Session correspondence is stored as terminal-visible records in the project's RoomKit timeline.
use crate::system_row::SystemRow;
use roomkit::model::{ChannelId, EventId, EventType, RoomEvent, RoomId};
use roomkit::{ConversationStore, RoomKit};
use roomkit_sqlite::SqliteStore;
use serde::{Deserialize, Serialize};

pub(super) const STREAM: &str = "roomkit_code:session_mail:v1";

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Message,
    ToSession {
        to: RoomId,
        agent: ChannelId,
        turn: String,
    },
    Reply(EventId),
    Notice(EventId),
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct Route {
    pub from: ChannelId,
    pub to: ChannelId,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct Mail {
    pub from: RoomId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<Route>,
    pub text: String,
    pub kind: Kind,
}

impl Mail {
    pub fn destination<'a>(&'a self, stored_in: &'a RoomId) -> &'a RoomId {
        match &self.kind {
            Kind::ToSession { to, .. } => to,
            _ => stored_in,
        }
    }
    pub fn line(&self) -> String {
        let kind = match &self.kind {
            Kind::ToSession { to, .. } => return format!("Message to @{to}\n{}", self.text),
            Kind::Message => "Message",
            Kind::Reply(_) => "Reply",
            Kind::Notice(_) => "Delivery notice",
        };
        format!("{kind} from @{}\n{}", self.from, self.text)
    }

    pub fn read(event: &RoomEvent) -> Option<Self> {
        if event.correlation_id.as_deref() != Some(STREAM)
            || event.r#type != EventType::System
            || event.source.channel_id.as_str() != crate::application::TERMINAL
        {
            return None;
        }
        serde_json::from_value(event.metadata.get(STREAM)?.clone()).ok()
    }

    pub async fn write(
        &self,
        kit: &RoomKit<SqliteStore>,
        dest: &RoomId,
        id: EventId,
        org: Option<String>,
    ) -> crate::Result<()> {
        self.commit(kit, dest, id, org, self.remote.is_some()).await
    }

    pub(super) async fn commit(
        &self,
        kit: &RoomKit<SqliteStore>,
        dest: &RoomId,
        id: EventId,
        org: Option<String>,
        single_outcome: bool,
    ) -> crate::Result<()> {
        let mut event = SystemRow {
            room: dest.as_str(),
            id: id.as_str(),
            text: &self.line(),
            about: (STREAM, serde_json::to_value(self)?),
        }
        .event(&ChannelId::new(crate::application::TERMINAL)?)?;
        event.correlation_id = Some(STREAM.into());
        // Concurrent outcomes for one question compete for one native
        // commit key. Their distinct event IDs still identify historical rows.
        event.idempotency_key = Some(match (single_outcome, &self.kind) {
            (true, Kind::Reply(original) | Kind::Notice(original)) => {
                format!("session-outcome-{original}")
            }
            _ => id.to_string(),
        });
        let options = roomkit::CommitEventOptions {
            organization_id: org,
        };
        if let Err(error) = kit.commit_event(dest, event, options).await {
            // A retry or simultaneous duplicate must refer to the identical envelope.
            let existing = kit.store().get_event(&id).await?;
            if !existing
                .is_some_and(|e| e.room_id == *dest && Self::read(&e).as_ref() == Some(self))
            {
                return Err(error.into());
            }
        }
        Ok(())
    }
}

pub(super) fn check_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty()
        || text.len() > 4000
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        return Err("Use 1–4000 bytes of message text".into());
    }
    Ok(())
}
