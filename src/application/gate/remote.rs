//! Authorize a worker tool call in the owner's conversation.
use super::{Gate, decide};
use crate::{Result, remote};
use roomkit::model::{ChannelType, HookAction, Room, RoomContext};
use roomkit::{RoomKit, ToolCallEvent, turns::DeclaredVerb};
use roomkit_sqlite::SqliteStore;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::sync::Arc;

#[derive(Deserialize)]
struct Request {
    name: String,
    arguments: Map<String, Value>,
    id: String,
}

pub(in crate::application) fn install(kit: &RoomKit<SqliteStore>, gate: Gate) -> Result<()> {
    for tool in [remote::pool::PeerTool::Ask, remote::pool::PeerTool::Answer] {
        install_peer(kit, gate.clone(), tool)?;
    }
    let verb = DeclaredVerb::new(move |fields| {
        let gate = gate.clone();
        async move {
            let request: Request = serde_json::from_value(Value::Object(fields))
                .map_err(|_| "Invalid worker tool request")?;
            Ok(match approved(&gate, request).await {
                Ok(_) => Value::Bool(true),
                Err(reason) => Value::String(reason),
            })
        }
    });
    Ok(kit.declare_host_verb(remote::AUTHORIZE, verb, Default::default())?)
}

fn install_peer(
    kit: &RoomKit<SqliteStore>,
    gate: Gate,
    tool: remote::pool::PeerTool,
) -> Result<()> {
    let verb = DeclaredVerb::new(move |args| {
        let gate = gate.clone();
        async move {
            let id = roomkit_ai::current_tool_call()
                .ok_or("Missing worker call")?
                .tool_call_id;
            let request = Request {
                name: tool.name().into(),
                arguments: args.clone(),
                id,
            };
            let room = approved(&gate, request).await?;
            let pool = gate
                .profiles
                .active_pool()
                .ok_or("Consultation needs an active pool")?;
            let kit = gate.kit.upgrade().ok_or("The conversation has closed")?;
            Ok(pool.consult(&kit, &room, tool, args).await?)
        }
    });
    Ok(kit.declare_host_verb(tool.verb(), verb, Default::default())?)
}

async fn approved(gate: &Gate, request: Request) -> std::result::Result<Arc<Room>, String> {
    // The engine supplies both identities from the accepted turn.
    let room = roomkit_ai::current_tool_room().ok_or("Missing worker room")?;
    let channel = roomkit_ai::current_tool_call()
        .and_then(|call| call.channel_id)
        .ok_or("Missing worker channel")?;
    let pooled = gate.profiles.active_pool().is_some();
    if channel.as_str() != remote::AGENT
        || room.metadata.get("task_agent_id").and_then(Value::as_str) != Some(remote::AGENT)
        || !gate.profiles.remote_authorized().await
        || !remote::permits(&request.name, pooled)
    {
        return Err("Authorization requires a permitted tool in a delegated worker room".into());
    }
    let req =
        remote::pool::Requirements::for_remote_task(room.metadata.get("task_context"), pooled)?;
    req.check_call(&request.name, &request.arguments)?;
    let mut event = ToolCallEvent::new(
        channel,
        ChannelType::AI,
        request.id,
        request.name,
        request.arguments,
    );
    event.room_id = Some(room.id.clone());
    let verdict = decide(gate, event, &RoomContext::new((*room).clone())).await;
    if verdict.action == HookAction::Allow {
        Ok(room)
    } else {
        Err(verdict
            .reason
            .unwrap_or_else(|| "Authorization refused".into()))
    }
}
