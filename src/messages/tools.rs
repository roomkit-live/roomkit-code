//! Agent-facing session discovery and asynchronous correspondence.
use super::{
    Messages,
    directory::{group_contacts, resolve},
    network::{AgentRequest, Network},
    record::{Kind, Mail, check_text},
    reply,
};
use roomkit::{
    RoomKit,
    model::{ChannelId, EventId, Metadata, RoomId},
};
use roomkit_ai::{AiTool, Tool, ToolError, ToolFuture, ToolResult};
use roomkit_sqlite::SqliteStore;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) const SEND_MESSAGE: &str = "send_message";
pub(crate) const LIST_SESSIONS: &str = "list_sessions";
type Kit = RoomKit<SqliteStore>;

impl Messages {
    pub(crate) fn tools(self: &Arc<Self>) -> Vec<Arc<dyn Tool>> {
        let Some(context) = self.interactive.get() else {
            return Vec::new();
        };
        let mut tools: Vec<Arc<dyn Tool>> = vec![Arc::new(SessionTool::Send(self.clone()))];
        if context.group_enabled {
            tools.push(Arc::new(SessionTool::List(self.clone())));
        }
        tools
    }

    fn group_for(&self, room: &RoomId) -> Result<&Network, ToolError> {
        self.network
            .get()
            .filter(|n| n.room == *room)
            .ok_or_else(|| ToolError::refused("This conversation has not joined a session group"))
    }
}

enum SessionTool {
    Send(Arc<Messages>),
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
                "List other open sessions in your trusted session group. Use their exact to address with send_message.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            Self::Send(m) if m.interactive.get().is_some_and(|c| c.group_enabled) => (
                SEND_MESSAGE,
                "Use to and text to start an asynchronous exchange with a session from list_sessions. Use reply_to and text to answer an incoming message. Give exactly one destination. Continue working after the send receipt.",
                json!({"type":"object","required":["text"],"additionalProperties":false,"properties":{"to":{"type":"string"},"reply_to":{"type":"string"},"text":{"type":"string","maxLength":4000}}}),
            ),
            Self::Send(_) => (
                SEND_MESSAGE,
                "Reply to an incoming session message using its message_id as reply_to. Include only information intended for its sender.",
                json!({"type":"object","required":["text","reply_to"],"additionalProperties":false,"properties":{"reply_to":{"type":"string"},"text":{"type":"string","maxLength":4000}}}),
            ),
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
            Self::Send(m) if name == SEND_MESSAGE => m.clone(),
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
                let peers = group_contacts(messages.group_for(&room.id)?)
                    .await
                    .map_err(ToolError::refused)?;
                json!({"sessions": peers.iter().map(|p| json!({"name":p.name,"to":p.key()})).collect::<Vec<_>>()})
            } else {
                send(&messages, &kit, &room.id, args).await?
            };
            Ok(ToolResult::Json(result))
        })
    }
}

async fn send(
    messages: &Messages,
    kit: &Kit,
    room: &RoomId,
    args: Metadata,
) -> Result<Value, ToolError> {
    let input: Input = serde_json::from_value(args.into())
        .map_err(|_| ToolError::refused("Use text and exactly one of to or reply_to"))?;
    check_text(&input.text).map_err(ToolError::refused)?;
    match (input.to, input.reply_to) {
        (Some(target), None) => initiate(messages.group_for(room)?, kit, &target, input.text).await,
        (None, Some(id)) => {
            let (event, mail) = reply::original(kit, room, id).await?;
            reply::respond(
                messages.network.get(),
                kit,
                &event,
                &mail,
                Kind::Reply(event.id.clone()),
                &input.text,
            )
            .await
            .map_err(|e| ToolError::refused(e.to_string()))?;
            Ok(json!({"status":"sent","reply_to":event.id}))
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
    let (id, request) = origin(net, peer, &text)?;
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

fn origin(
    net: &Network,
    peer: &ChannelId,
    text: &str,
) -> Result<(EventId, AgentRequest), ToolError> {
    let call = roomkit_ai::current_tool_call()
        .filter(|c| c.room_id.as_ref() == Some(&net.room) && !c.tool_call_id.is_empty())
        .ok_or_else(|| ToolError::refused("Missing calling session"))?;
    let agent = call
        .channel_id
        .ok_or_else(|| ToolError::refused("Missing calling agent"))?;
    let turn =
        roomkit::current_turn_id().ok_or_else(|| ToolError::refused("Missing calling turn"))?;
    let identity = json!([
        "agent-message",
        net.channel,
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
