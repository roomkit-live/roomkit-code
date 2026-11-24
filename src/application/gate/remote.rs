//! Authorize a worker tool call in the owner's conversation.

use super::{Gate, decide};
use crate::{Result, remote};
use roomkit::model::{ChannelType, HookAction, Room, RoomContext};
use roomkit::{RoomKit, ToolCallEvent};
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
    let permissions = gate.clone();
    let verb = roomkit::turns::DeclaredVerb::new(move |mut fields| {
        let gate = permissions.clone();
        async move {
            let reports = fields.remove("report_outcomes") == Some(Value::Bool(true));
            let (event, room, turn) = worker_call(fields, &gate).await?;
            let receipt = reports
                .then(|| gate.hooks.request_worker_report(&event, &room, &turn))
                .flatten();
            let verdict = decide(
                &gate,
                event,
                &RoomContext::new((*room).clone()),
                Some(&turn),
            )
            .await;
            let decision = if verdict.action == HookAction::Allow {
                Value::Bool(true)
            } else {
                Value::String(
                    verdict
                        .reason
                        .unwrap_or_else(|| "Authorization refused".into()),
                )
            };
            Ok(if let Some(receipt) = receipt {
                serde_json::json!({"decision": decision, "report_receipt": receipt})
            } else {
                decision
            })
        }
    });
    kit.declare_host_verb(remote::AUTHORIZE, verb, Default::default())?;
    let outcome = roomkit::turns::DeclaredVerb::new(move |mut fields| {
        let gate = gate.clone();
        async move {
            let report: remote::ToolOutcome =
                serde_json::from_value(fields.remove("outcome").ok_or("Missing tool outcome")?)
                    .map_err(|_| "Invalid tool outcome")?;
            let receipt = fields
                .get("receipt")
                .and_then(Value::as_str)
                .ok_or("Missing tool report receipt")?;
            let (room, _) = worker_identity(&gate).await?;
            gate.hooks
                .finish_worker_report(&room, receipt, report)
                .await?;
            Ok(Value::Null)
        }
    });
    Ok(kit.declare_host_verb(
        remote::OUTCOME,
        outcome,
        roomkit::turns::DeclareHostVerbOptions {
            between_turns: true,
        },
    )?)
}

async fn worker_call(
    fields: Map<String, Value>,
    gate: &Gate,
) -> std::result::Result<(ToolCallEvent, Arc<Room>, String), String> {
    let request: Request =
        serde_json::from_value(Value::Object(fields)).map_err(|_| "Invalid worker tool request")?;
    let (room, channel) = worker_identity(gate).await?;
    if !remote::permits(&request.name, gate.profiles.active_pool().is_some()) {
        return Err("This tool is unavailable on the remote worker".into());
    }
    let mut event = ToolCallEvent::new(
        channel,
        ChannelType::AI,
        request.id,
        request.name,
        request.arguments,
    );
    event.room_id = Some(room.id.clone());
    let turn = roomkit::current_turn_id().ok_or("Missing worker turn")?;
    Ok((event, room, turn))
}

async fn worker_identity(
    gate: &Gate,
) -> std::result::Result<(Arc<Room>, roomkit::model::ChannelId), String> {
    // The engine supplies both identities from the accepted turn.
    let room = roomkit_ai::current_tool_room().ok_or("Missing worker room")?;
    let channel = roomkit_ai::current_tool_call()
        .and_then(|call| call.channel_id)
        .ok_or("Missing worker channel")?;
    if channel.as_str() != remote::AGENT
        || room.metadata.get("task_agent_id").and_then(Value::as_str) != Some(remote::AGENT)
        || !gate.profiles.remote_authorized().await
    {
        return Err("Authorization requires a permitted tool in a delegated worker room".into());
    }
    Ok((room, channel))
}
