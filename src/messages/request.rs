//! Bind a session question to the agent and turn that will receive its answer.
use super::Kit;
use super::record::{Kind, Mail};
use roomkit::{
    ConversationStore,
    model::{ChannelId, EventId, RoomId},
};
use roomkit_ai::ToolError;
use serde_json::json;

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct AgentRequest {
    pub agent: ChannelId,
    pub turn: String,
    pub question: String,
}

pub(super) fn origin(
    room: &RoomId,
    sender: &str,
    peer: &str,
    text: &str,
) -> Result<(EventId, AgentRequest), ToolError> {
    let call = roomkit_ai::current_tool_call()
        .filter(|c| c.room_id.as_ref() == Some(room) && !c.tool_call_id.is_empty())
        .ok_or_else(|| ToolError::refused("Missing calling session"))?;
    let agent = call
        .channel_id
        .ok_or_else(|| ToolError::refused("Missing calling agent"))?;
    let turn =
        roomkit::current_turn_id().ok_or_else(|| ToolError::refused("Missing calling turn"))?;
    let identity = json!([
        "agent-message",
        sender,
        agent,
        turn,
        call.tool_call_id,
        peer,
        text
    ]);
    let hash = crate::digest::sha256_hex(identity.to_string().as_bytes());
    let id = EventId::new(format!("session-msg-{hash}"))
        .map_err(|e| ToolError::refused(e.to_string()))?;
    let request = AgentRequest {
        agent,
        turn,
        question: text.to_owned(),
    };
    Ok((id, request))
}

/// A local reply may resume only the agent that wrote the request to this child.
pub(super) async fn local(
    kit: &Kit,
    event: &roomkit::model::RoomEvent,
    mail: &Mail,
    original: &roomkit::model::EventId,
) -> crate::Result<Option<AgentRequest>> {
    let Some(row) = kit
        .store()
        .get_event(original)
        .await?
        .filter(|r| r.room_id == event.room_id)
    else {
        return Ok(None);
    };
    let Some(Mail {
        kind: Kind::ToSession { to, agent, turn },
        text,
        from,
        remote: None,
    }) = Mail::read(&row)
    else {
        return Ok(None);
    };
    Ok(
        (to == mail.from && from == event.room_id).then_some(AgentRequest {
            agent,
            turn,
            question: text,
        }),
    )
}
