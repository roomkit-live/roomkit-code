//! Authorize a worker tool call in the owner's conversation.

use super::{Gate, decide};
use crate::{Result, remote};
use roomkit::model::{ChannelType, HookAction, Room, RoomContext};
use roomkit::{RoomKit, ToolCallEvent};
use roomkit_providers_sqlite::SqliteStore;
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
    for action in [
        WorkerAction::Peer(remote::pool::PeerTool::Ask),
        WorkerAction::Peer(remote::pool::PeerTool::Answer),
        WorkerAction::Reply,
    ] {
        install_action(kit, gate.clone(), action)?;
    }
    install_authorization(kit, gate.clone())?;
    install_outcomes(kit, gate)
}

fn install_authorization(kit: &RoomKit<SqliteStore>, permissions: Gate) -> Result<()> {
    let verb = roomkit::turns::DeclaredVerb::new(move |mut fields| {
        let gate = permissions.clone();
        async move {
            let reports = fields.remove("report_outcomes") == Some(Value::Bool(true));
            let request = serde_json::from_value(Value::Object(fields))
                .map_err(|_| "Invalid worker tool request")?;
            let (event, room, turn) = match worker_call(request, &gate).await {
                Ok(call) => call,
                Err(reason) => return Ok(Value::String(reason)),
            };
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
    Ok(kit.declare_host_verb(remote::AUTHORIZE, verb, Default::default())?)
}

fn install_outcomes(kit: &RoomKit<SqliteStore>, gate: Gate) -> Result<()> {
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

#[derive(Clone, Copy)]
enum WorkerAction {
    Peer(remote::pool::PeerTool),
    Reply,
}
impl WorkerAction {
    fn names(self) -> (&'static str, &'static str) {
        match self {
            Self::Peer(peer) => (peer.name(), peer.verb()),
            Self::Reply => (
                crate::names::SEND_MESSAGE,
                crate::messages::WorkerReply::VERB,
            ),
        }
    }
    async fn run(
        self,
        gate: &Gate,
        room: &Room,
        args: Map<String, Value>,
    ) -> std::result::Result<Value, String> {
        match self {
            Self::Peer(peer) => consult(gate, room, peer, args).await,
            Self::Reply => {
                let kit = gate.kit.upgrade().ok_or("The conversation has closed")?;
                crate::messages::WorkerReply::respond(&kit, room, args).await
            }
        }
    }
}

fn install_action(kit: &RoomKit<SqliteStore>, gate: Gate, action: WorkerAction) -> Result<()> {
    let (name, verb_name) = action.names();
    let verb = roomkit::turns::DeclaredVerb::new(move |args| {
        let gate = gate.clone();
        async move {
            let id = roomkit_ai::current_tool_call()
                .ok_or("Missing worker call")?
                .tool_call_id;
            let request = Request {
                name: name.into(),
                arguments: args.clone(),
                id,
            };
            let (event, room, turn) = worker_call(request, &gate).await?;
            let receipt = gate.hooks.request_worker_report(&event, &room, &turn);
            let verdict = decide(
                &gate,
                event,
                &RoomContext::new((*room).clone()),
                Some(&turn),
            )
            .await;
            let refused = verdict.action != HookAction::Allow;
            let result = if refused {
                Err(verdict
                    .reason
                    .unwrap_or_else(|| "Authorization refused".into()))
            } else {
                action.run(&gate, &room, args).await
            };
            if let Some(receipt) = receipt {
                let outcome = remote::ToolOutcome::from_owner_action(&result, refused);
                gate.hooks
                    .finish_worker_report(&room, &receipt, outcome)
                    .await?;
            }
            Ok(result?)
        }
    });
    Ok(kit.declare_host_verb(verb_name, verb, Default::default())?)
}

async fn consult(
    gate: &Gate,
    room: &Room,
    tool: remote::pool::PeerTool,
    args: Map<String, Value>,
) -> std::result::Result<Value, String> {
    let pool = gate
        .profiles
        .active_pool()
        .ok_or("Consultation needs an active pool")?;
    let kit = gate.kit.upgrade().ok_or("The conversation has closed")?;
    pool.consult(&kit, room, tool, args).await
}

async fn worker_call(
    request: Request,
    gate: &Gate,
) -> std::result::Result<(ToolCallEvent, Arc<Room>, String), String> {
    let (room, channel) = worker_identity(gate).await?;
    let pooled = gate.profiles.active_pool().is_some();
    if !remote::permits(&request.name, pooled) {
        return Err("This tool is unavailable on the remote worker".into());
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
