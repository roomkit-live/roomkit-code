//! Verify the original message before sending anything back to its origin.
use super::record::{Kind, Mail};
use roomkit::model::{EventId, RoomEvent, RoomId};
use roomkit::{ConversationStore, RoomKit};
use roomkit_ai::ToolError;
use roomkit_sqlite::SqliteStore;

pub(super) async fn original(
    kit: &RoomKit<SqliteStore>,
    room: &RoomId,
    id: String,
) -> Result<(RoomEvent, Mail), ToolError> {
    let id = EventId::new(id)
        .ok()
        .filter(|id| id.as_str().starts_with("session-msg-"))
        .ok_or_else(|| ToolError::refused("Invalid session message ID"))?;
    let event = kit
        .store()
        .get_event(&id)
        .await
        .map_err(|e| ToolError::refused(e.to_string()))?
        .ok_or_else(|| ToolError::refused("No such message in this session"))?;
    let mail = Mail::read(&event)
        .filter(|m| {
            matches!(m.kind, Kind::Message | Kind::ToSession { .. })
                && m.destination(&event.room_id) == room
        })
        .ok_or_else(|| ToolError::refused("This event does not accept a reply"))?;
    Ok((event, mail))
}

pub(super) fn reply_id(id: &EventId) -> crate::Result<EventId> {
    Ok(EventId::new(format!("session-reply-{id}"))?)
}

pub(super) fn response_id(kind: &Kind) -> crate::Result<EventId> {
    match kind {
        Kind::Reply(id) => reply_id(id),
        Kind::Notice(id) => Ok(EventId::new(format!("session-notice-{id}"))?),
        Kind::Message | Kind::ToSession { .. } => {
            Err("Expected a session reply or delivery notice".into())
        }
    }
}

/// The peer accepted the response in both cases. Its local receipt may
/// still need reconciliation when the receiving turn finishes.
pub(super) enum Acknowledgement {
    Saved,
    LocalWritePending,
}

pub(super) async fn respond(
    net: Option<&super::network::Network>,
    kit: &RoomKit<SqliteStore>,
    event: &RoomEvent,
    original: &Mail,
    kind: Kind,
    text: &str,
) -> crate::Result<Acknowledgement> {
    let room = kit.get_room(&event.room_id, Default::default()).await?;
    let response = Mail {
        from: original.destination(&event.room_id).clone(),
        text: text.into(),
        remote: None,
        kind,
    };
    if let Some(route) = &original.remote {
        return net
            .ok_or("Remote session network unavailable")?
            .reply(kit, route, &response)
            .await;
    }
    let destination = kit
        .get_room(
            &original.from,
            roomkit::GetRoomOptions {
                organization_id: room.organization_id.clone(),
            },
        )
        .await?;
    if room.organization_id != destination.organization_id
        || !super::writable(kit, &destination.id, destination.organization_id).await?
    {
        return Err("The sending session cannot receive this reply".into());
    }
    let id = response_id(&response.kind)?;
    let single = matches!(original.kind, Kind::ToSession { .. });
    if let Err(error) = response
        .commit(kit, &original.from, id, room.organization_id, single)
        .await
        && !(single
            && matches!(response.kind, Kind::Notice(_))
            && super::delivery::has_local_reply(kit, event).await?)
    {
        return Err(error);
    }
    Ok(Acknowledgement::Saved)
}
