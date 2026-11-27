//! Resolve a live mailbox's display title through an addressed, read-only RoomKit call.
//! Both proofs bind the mailbox and a fresh nonce. Older peers remain reachable by ID.
use super::{Kit, Network, digest, seal::Guard};
use roomkit::model::{ChannelId, EventId, Metadata, RoomId};
use roomkit::turns::{ChannelCall, TurnBackend, wire::ChannelControl};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::task::JoinSet;

pub(super) const VERB: &str = "read_session_title";
const PREFIX: &str = "session-title-";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    id: EventId,
    seal: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    title: Option<String>,
    seal: Option<String>,
}

fn request(channel: &ChannelId) -> Value {
    json!([VERB, "request", channel])
}

fn response(channel: &ChannelId, title: &Option<String>) -> Value {
    json!([VERB, "response", channel, title])
}

pub(super) async fn answer(
    kit: &Kit,
    room: &RoomId,
    channel: &ChannelId,
    guard: &Guard,
    args: Metadata,
) -> crate::Result<Value> {
    let query: Query = serde_json::from_value(args.into())?;
    if !query
        .id
        .as_str()
        .strip_prefix(PREFIX)
        .is_some_and(|s| digest(s, 32))
        || !guard.authentic(&query.id, &request(channel), query.seal.as_deref())
    {
        return Err("Invalid session title request".into());
    }
    let title = crate::sessions::name(kit, room).await;
    let seal = guard.seal(&query.id, &response(channel, &title))?;
    Ok(json!(Reply { title, seal }))
}

impl Network {
    async fn peer_title(&self, channel: &ChannelId, room: RoomId) -> Option<String> {
        let id = EventId::new(format!("{PREFIX}{}", uuid::Uuid::new_v4().simple())).ok()?;
        let seal = self.guard.seal(&id, &request(channel)).ok()?;
        let mut call = ChannelCall::new(channel.clone(), room, VERB);
        call.arguments = serde_json::from_value(json!(Query {
            id: id.clone(),
            seal
        }))
        .ok()?;
        let reply = self
            .backend
            .control(ChannelControl::ChannelCall(Box::new(call)))
            .await
            .ok()??;
        if reply.error.is_some() {
            return None;
        }
        let reply: Reply = serde_json::from_value(reply.result).ok()?;
        if !self
            .guard
            .authentic(&id, &response(channel, &reply.title), reply.seal.as_deref())
        {
            return None;
        }
        crate::sessions::names::parse(&reply.title?).ok()
    }

    /// Discovery decides availability; a missing or slow title never removes that peer.
    pub(in crate::messages) async fn titles(
        &self,
        peers: &[(ChannelId, RoomId)],
    ) -> HashMap<ChannelId, String> {
        let mut names = HashMap::new();
        let mut pending = JoinSet::new();
        let mut peers = peers.iter().cloned();
        let lookup = async {
            loop {
                while pending.len() < 8 {
                    let Some((channel, room)) = peers.next() else {
                        break;
                    };
                    let net = self.clone();
                    pending.spawn(async move {
                        let title = net.peer_title(&channel, room).await?;
                        Some((channel, title))
                    });
                }
                match pending.join_next().await {
                    None => break,
                    Some(Ok(Some((channel, title)))) => {
                        names.insert(channel, title);
                    }
                    _ => {}
                }
            }
        };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), lookup).await;
        names
    }
}
