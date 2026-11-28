//! Agent-facing session discovery and asynchronous correspondence.
use super::{
    Messages,
    directory::{group_contacts, resolve},
    network::Network,
    record::{Kind, Mail, check_text},
    reply,
    request::origin,
};
use roomkit::model::{Metadata, RoomId};
use roomkit_ai::{AiTool, Tool, ToolError, ToolFuture, ToolResult};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use super::Kit;
pub(crate) use crate::names::{LIST_SESSIONS, SEND_MESSAGE};

impl Messages {
    pub(crate) fn tools(self: &Arc<Self>, pool_enabled: bool) -> Vec<Arc<dyn Tool>> {
        let Some(context) = self.interactive.get() else {
            return Vec::new();
        };
        let mut tools: Vec<Arc<dyn Tool>> =
            vec![Arc::new(SessionTool::Send(self.clone(), pool_enabled))];
        if context.group_enabled || pool_enabled {
            tools.push(Arc::new(SessionTool::List(self.clone())));
        }
        tools
    }

    async fn sessions(&self, kit: &Kit, room: &roomkit::model::Room) -> Result<Value, ToolError> {
        let mut sessions = if self.pool().is_some() {
            super::delegated::contacts(kit, room)
                .await
                .map_err(|e| ToolError::refused(e.to_string()))?
        } else {
            Vec::new()
        };
        match self.group_sessions(&room.id).await {
            Ok(peers) => sessions.extend(peers),
            Err(error) if sessions.is_empty() => return Err(ToolError::refused(error)),
            Err(error) => return Ok(json!({"sessions":sessions,"group_error":error})),
        }
        Ok(json!({"sessions":sessions}))
    }

    /// The sessions of `room`'s group; none for a conversation that joined
    /// no group. A group this launch could not reach is an error with its
    /// reason, as `send_message` gives it: a list without sessions would
    /// read as a group nobody is in.
    async fn group_sessions(&self, room: &RoomId) -> Result<Vec<Value>, String> {
        let unreachable = std::sync::atomic::Ordering::Acquire;
        let net = match self.group_for(room) {
            Ok(net) => net,
            Err(ToolError::Refused(reason)) if self.network_unavailable.load(unreachable) => {
                return Err(reason);
            }
            Err(_) => return Ok(Vec::new()),
        };
        let peers = group_contacts(net).await?;
        Ok(peers
            .iter()
            .map(|p| json!({"name":p.room,"title":p.name,"to":p.key()}))
            .collect())
    }

    fn group_for(&self, room: &RoomId) -> Result<&Network, ToolError> {
        self.network
            .get()
            .filter(|n| n.room == *room)
            .ok_or_else(|| {
                let unavailable = self
                    .network_unavailable
                    .load(std::sync::atomic::Ordering::Acquire);
                ToolError::refused(if unavailable {
                    "The session group could not be reached when this launch started. \
                 Relaunch once its Redis is reachable"
                } else {
                    "This conversation has not joined a session group"
                })
            })
    }
}

enum SessionTool {
    Send(Arc<Messages>, bool),
    List(Arc<Messages>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    to: Option<String>,
    reply_to: Option<String>,
    text: String,
}

impl Tool for SessionTool {
    fn definition(&self) -> AiTool {
        let (name, description, parameters) = match self {
            Self::List(_) => (
                LIST_SESSIONS,
                "List the sessions this conversation can contact, including its delegated executor sessions. Copy the exact to address into send_message.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            Self::Send(m, pool)
                if *pool || m.interactive.get().is_some_and(|c| c.group_enabled) =>
            {
                (
                    SEND_MESSAGE,
                    "Write to a session using its exact to address from list_sessions or the child_room_id returned by delegate_task. Supply text. For a reply use reply_to with the incoming message ID instead of to. Continue working after sending.",
                    json!({"type":"object","required":["text"],"additionalProperties":false,"properties":{"to":{"type":"string","description":"Recipient for a new message. Leave empty when replying."},"reply_to":{"type":"string","description":"Received message ID for a reply. Leave empty for a new message."},"text":{"type":"string","maxLength":4000}}}),
                )
            }
            Self::Send(..) => return reply_definition(),
        };
        AiTool {
            name: name.into(),
            description: description.into(),
            parameters,
            changes_state: false,
            tags: vec![],
            defer_loading: false,
        }
    }

    fn handler(&self, name: String, args: Metadata) -> ToolFuture {
        let messages = match self {
            Self::Send(m, _) if name == SEND_MESSAGE => m.clone(),
            Self::List(m) if name == LIST_SESSIONS => m.clone(),
            _ => return Box::pin(std::future::ready(Err(ToolError::unserved(name)))),
        };
        Box::pin(async move {
            let room = roomkit_ai::current_tool_room()
                .ok_or_else(|| ToolError::refused("Missing session"))?;
            let kit = messages
                .kit()
                .ok_or_else(|| ToolError::refused("Session unavailable"))?;
            if !super::writable(&kit, &room.id, room.organization_id.clone())
                .await
                .map_err(ToolError::refused)?
            {
                return Err(ToolError::refused("The session is read-only"));
            }
            let result = if name == LIST_SESSIONS {
                if !args.is_empty() {
                    return Err(ToolError::refused("list_sessions takes no arguments"));
                }
                messages.sessions(&kit, &room).await?
            } else {
                send(&messages, &kit, &room, args).await?
            };
            Ok(ToolResult::Json(result))
        })
    }
}

async fn send(
    messages: &Messages,
    kit: &Kit,
    room: &roomkit::model::Room,
    args: Metadata,
) -> Result<Value, ToolError> {
    let input: Input = serde_json::from_value(args.into())
        .map_err(|_| ToolError::refused("Use text and exactly one of to or reply_to"))?;
    check_text(&input.text).map_err(ToolError::refused)?;
    // Some providers fill an unused optional string with an empty value.
    let to = input.to.filter(|s| !s.trim().is_empty());
    let reply_to = input.reply_to.filter(|s| !s.trim().is_empty());
    match (to, reply_to) {
        (Some(target), None) => {
            if let Some(task) = target.strip_prefix("task:") {
                let pool = messages.pool().ok_or_else(|| {
                    ToolError::refused("This conversation has no active executor pool")
                })?;
                return pool
                    .send_to_task(kit, room, task, &input.text)
                    .await
                    .map_err(ToolError::refused);
            }
            if messages.pool().is_some() && !target.starts_with("peer:") {
                if let Ok(id) = RoomId::new(target.as_str())
                    && let Some(child) = super::delegated::destination(kit, room, &id)
                        .await
                        .map_err(|e| ToolError::refused(e.to_string()))?
                {
                    return super::delegated::send(kit, room, child, input.text).await;
                }
                if messages.group_for(&room.id).is_err() {
                    return Err(ToolError::refused(
                        "Session unavailable. Choose an exact address from list_sessions.",
                    ));
                }
            }
            initiate(messages.group_for(&room.id)?, kit, &target, input.text).await
        }
        (None, Some(id)) => {
            let (event, mail) = reply::original(kit, &room.id, id).await?;
            let ack = reply::respond(
                messages.network.get(),
                kit,
                &event,
                &mail,
                Kind::Reply(event.id.clone()),
                &input.text,
            )
            .await
            .map_err(|e| ToolError::refused(e.to_string()))?;
            let mut result = json!({"status":"sent","reply_to":event.id});
            if matches!(ack, reply::Acknowledgement::LocalWritePending) {
                result["local_receipt"] = json!("pending");
            }
            Ok(result)
        }
        _ => Err(ToolError::refused("Use exactly one of to or reply_to")),
    }
}

async fn initiate(
    net: &Network,
    kit: &Kit,
    target: &str,
    text: String,
) -> Result<Value, ToolError> {
    let peers = group_contacts(net).await.map_err(ToolError::refused)?;
    let peer = resolve(&peers, target)
        .and_then(|p| p.remote.as_ref())
        .ok_or_else(|| {
            ToolError::refused(
                "Recipient unavailable or ambiguous. Use list_sessions to choose an exact address.",
            )
        })?;
    let (id, request) = origin(&net.room, net.channel.as_str(), peer.as_str(), &text)?;
    let mail = Mail {
        from: net.room.clone(),
        remote: None,
        text,
        kind: Kind::Message,
    };
    net.request(kit, peer, &id, &mail, Some(request))
        .await
        .map_err(|e| ToolError::refused(e.to_string()))?;
    Ok(
        json!({"status":"sent","message_id":id,"to":target,"delivery":"Continue working. An explicit reply arrives asynchronously."}),
    )
}

pub(super) fn reply_definition() -> AiTool {
    AiTool {
        name: SEND_MESSAGE.into(),
        description: "Reply to an incoming session message using its message_id as reply_to. Include only information intended for its sender.".into(),
        parameters: json!({"type":"object","required":["text","reply_to"],"additionalProperties":false,"properties":{"reply_to":{"type":"string"},"text":{"type":"string","maxLength":4000}}}),
        changes_state:false, tags: Vec::new(), defer_loading:false,
    }
}
