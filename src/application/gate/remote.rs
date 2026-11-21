//! Authorize a worker tool call in the owner's conversation.

use super::{Gate, decide};
use crate::{Result, remote};
use roomkit::model::{ChannelType, HookAction, RoomContext};
use roomkit::{RoomKit, ToolCallEvent};
use roomkit_sqlite::SqliteStore;
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Deserialize)]
struct Request {
    name: String,
    arguments: Map<String, Value>,
    id: String,
}

pub(in crate::application) fn install(kit: &RoomKit<SqliteStore>, gate: Gate) -> Result<()> {
    let verb = roomkit::turns::DeclaredVerb::new(move |fields| {
        let gate = gate.clone();
        async move {
            let request: Request = serde_json::from_value(Value::Object(fields))
                .map_err(|_| "Invalid worker tool request")?;
            // The engine supplies both identities from the accepted turn.
            let room = roomkit_ai::current_tool_room().ok_or("Missing worker room")?;
            let channel = roomkit_ai::current_tool_call()
                .and_then(|call| call.channel_id)
                .ok_or("Missing worker channel")?;
            if channel.as_str() != remote::AGENT
                || room.metadata.get("task_agent_id").and_then(Value::as_str) != Some(remote::AGENT)
                || !remote::permits(&request.name, gate.profiles.pool.is_some())
            {
                return Err(
                    "Authorization requires a permitted tool in a delegated worker room".into(),
                );
            }
            let mut event = ToolCallEvent::new(
                channel,
                ChannelType::AI,
                request.id,
                request.name,
                request.arguments,
            );
            event.room_id = Some(room.id.clone());
            let verdict = decide(&gate, event, &RoomContext::new((*room).clone())).await;
            Ok(if verdict.action == HookAction::Allow {
                Value::Bool(true)
            } else {
                Value::String(
                    verdict
                        .reason
                        .unwrap_or_else(|| "Authorization refused".into()),
                )
            })
        }
    });
    Ok(kit.declare_host_verb(remote::AUTHORIZE, verb, Default::default())?)
}
