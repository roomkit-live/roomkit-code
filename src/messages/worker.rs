//! The room owner verifies replies from worker turns against their addressed mail.
use super::{
    record::{Kind, check_text},
    reply,
};
use roomkit::{
    RoomKit,
    model::{Metadata, Room},
};
use roomkit_ai::{AiTool, Tool, ToolError, ToolFuture, ToolResult};
use roomkit_sqlite::SqliteStore;
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) struct WorkerReply;

impl WorkerReply {
    pub(crate) const VERB: &str = "code_reply_session_message";

    pub(crate) async fn respond(
        kit: &RoomKit<SqliteStore>,
        room: &Room,
        args: Metadata,
    ) -> Result<Value, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Answer {
            reply_to: String,
            text: String,
        }
        let answer: Answer =
            serde_json::from_value(args.into()).map_err(|_| "Use reply_to and text")?;
        check_text(&answer.text)?;
        if !super::writable(kit, &room.id, room.organization_id.clone()).await? {
            return Err("The session is read-only".into());
        }
        let (event, mail) = reply::original(kit, &room.id, answer.reply_to)
            .await
            .map_err(|e| e.to_string())?;
        if !matches!(mail.kind, Kind::ToSession { .. })
            || mail.remote.is_some()
            || room.metadata.get("parent_room_id") != Some(&json!(event.room_id))
            || mail.from != event.room_id
        {
            return Err("No such question from the delegating session".into());
        }
        reply::respond(
            None,
            kit,
            &event,
            &mail,
            Kind::Reply(event.id.clone()),
            &answer.text,
        )
        .await
        .map_err(|e| e.to_string())?;
        Ok(json!({"status":"sent","reply_to":event.id}))
    }
}
impl Tool for WorkerReply {
    fn definition(&self) -> AiTool {
        super::tools::reply_definition()
    }
    fn handler(&self, name: String, args: Metadata) -> ToolFuture {
        Box::pin(async move {
            if name != super::SEND_MESSAGE {
                return Err(ToolError::unserved(name));
            }
            let host = roomkit::current_turn_host()
                .ok_or_else(|| ToolError::refused("No session owner is available"))?;
            host.call(Self::VERB, Some(args))
                .await
                .map(ToolResult::Json)
                .map_err(|e| ToolError::refused(e.to_string()))
        })
    }
}
