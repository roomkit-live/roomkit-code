//! Return a reply only to the verified origin of a message in the calling room.
use super::{
    Messages,
    record::{Kind, Mail, check_text},
};
use roomkit::model::{EventId, Metadata, RoomEvent, RoomId};
use roomkit::{ConversationStore, RoomKit, WeakRoomKit};
use roomkit_ai::{AiTool, Tool, ToolError, ToolFuture, ToolResult};
use roomkit_sqlite::SqliteStore;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

pub(crate) const NAME: &str = "send_message";
pub(crate) struct Reply(pub Arc<Messages>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    reply_to: String,
    text: String,
}

impl Tool for Reply {
    fn definition(&self) -> AiTool {
        AiTool {
            name: NAME.into(),
            description: "Reply to an incoming session message. Use the message_id supplied by RoomKit Code and include only the information intended for its sender.".into(),
            parameters: json!({"type":"object","required":["reply_to","text"],"additionalProperties":false,"properties":{"reply_to":{"type":"string"},"text":{"type":"string","maxLength":4000}}}),
            changes_state: false, tags: vec![], defer_loading: false,
        }
    }

    fn handler(&self, name: String, args: Metadata) -> ToolFuture {
        let messages = self.0.clone();
        Box::pin(async move {
            if name != NAME {
                return Err(ToolError::unserved(name));
            }
            let input: Input = serde_json::from_value(args.into())
                .map_err(|_| ToolError::refused("Use reply_to and text"))?;
            check_text(&input.text).map_err(ToolError::refused)?;
            let room = roomkit_ai::current_tool_room()
                .ok_or_else(|| ToolError::refused("Missing session"))?;
            let kit = messages
                .kit
                .get()
                .and_then(WeakRoomKit::upgrade)
                .ok_or_else(|| ToolError::refused("Session unavailable"))?;
            if !super::writable(&kit, &room.id, room.organization_id.clone())
                .await
                .map_err(ToolError::refused)?
            {
                return Err(ToolError::refused("The session is read-only"));
            }
            let (event, mail) = original(&kit, &room.id, input.reply_to).await?;
            respond(
                &kit,
                &event,
                &mail,
                Kind::Reply(event.id.clone()),
                &input.text,
            )
            .await
            .map_err(|e| ToolError::refused(e.to_string()))?;
            Ok(ToolResult::Json(
                json!({"status":"sent","reply_to":event.id}),
            ))
        })
    }
}

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
    kit: &RoomKit<SqliteStore>,
    event: &RoomEvent,
    original: &Mail,
    kind: Kind,
    text: &str,
) -> crate::Result<()> {
    let room = kit.get_room(&event.room_id, Default::default()).await?;
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
    let id = match &kind {
        Kind::Reply(id) => reply_id(id)?,
        Kind::Notice(id) => EventId::new(format!("session-notice-{id}"))?,
        Kind::Message => return Err("Expected a session reply or delivery notice".into()),
    };
    Mail {
        from: room.id,
        text: text.into(),
        kind,
    }
    .write(kit, &original.from, id, room.organization_id)
    .await
}
