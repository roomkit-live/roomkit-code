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
        .filter(|e| &e.room_id == room)
        .ok_or_else(|| ToolError::refused("No such message in this session"))?;
    let mail = Mail::read(&event)
        .filter(|m| m.kind == Kind::Message)
        .ok_or_else(|| ToolError::refused("This event does not accept a reply"))?;
    Ok((event, mail))
}

pub(super) fn reply_id(id: &EventId) -> crate::Result<EventId> {
    Ok(EventId::new(format!("session-reply-{id}"))?)
}

pub(super) async fn respond(
    net: Option<&super::network::Network>,
    kit: &RoomKit<SqliteStore>,
    event: &RoomEvent,
    original: &Mail,
    kind: Kind,
    text: &str,
) -> crate::Result<()> {
    let room = kit.get_room(&event.room_id, Default::default()).await?;
    let id = match &kind {
        Kind::Reply(id) => reply_id(id)?,
        Kind::Notice(id) => EventId::new(format!("session-notice-{id}"))?,
        Kind::Message => return Err("Expected a session reply or delivery notice".into()),
    };
    let response = Mail {
        from: room.id.clone(),
        text: text.into(),
        remote: None,
        kind,
    };
    if let Some(route) = &original.remote {
        return net
            .ok_or("Remote session network unavailable")?
            .reply(kit, route, &id, &response)
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
    response
        .write(kit, &original.from, id, room.organization_id)
        .await
}
