//! A named interactive session advertises one mailbox using RoomKit channel calls.
//! The broker carries mail between independent databases. Each owner delivers locally.
mod channel;
mod recovery;
mod response;
use super::{
    record::{Mail, Route},
    request::AgentRequest,
};
use crate::{Result, storage::SessionLeases};
use roomkit::model::{ChannelId, EventId, RoomId};
use roomkit::turns::{ChannelCall, TurnBackend, wire::ChannelControl};
use roomkit::{
    ConversationStore, RedisTurnBackend, RedisTurnBackendOptions, TurnExecutor, TurnExecutorOptions,
};
use serde_json::json;
use std::sync::Arc;

use super::Kit;
pub(super) const ADDRESS: &str = "roomkit_code:session_address:v1";
const PREFIX: &str = "session-mail-";
const VERB: &str = "receive_session_message";

/// The random part identifies a live process even if it reopens the same room.
pub(super) fn room_of(channel: &ChannelId) -> Option<RoomId> {
    let (instance, name) = channel.as_str().strip_prefix(PREFIX)?.split_once('-')?;
    if !digest(instance, 32) || !crate::config::valid_session_name(name) {
        return None;
    }
    RoomId::new(name).ok()
}

fn digest(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone)]
pub(super) struct Network {
    backend: RedisTurnBackend,
    pub room: RoomId,
    pub channel: ChannelId,
    scope: String,
    pub wake: Arc<tokio::sync::Notify>,
    responses: Arc<tokio::sync::Mutex<()>>,
}

impl Network {
    pub async fn open(
        group: &str,
        address: &str,
        kit: &Kit,
        room: &RoomId,
        leases: Arc<SessionLeases>,
    ) -> Result<Self> {
        let backend = RedisTurnBackend::new(
            Some(address),
            RedisTurnBackendOptions {
                key_prefix: format!("roomkit:code:session-mail:v1:{group}"),
                ..Default::default()
            },
        )?;
        let channel = ChannelId::new(format!("{PREFIX}{}-{room}", uuid::Uuid::new_v4().simple()))?;
        let wake = Arc::new(tokio::sync::Notify::new());
        let inbox = channel::Inbox::new(
            kit.downgrade(),
            room.clone(),
            channel.clone(),
            leases,
            wake.clone(),
        );
        let executor = TurnExecutor::new(
            move |id| std::future::ready(Ok((id == inbox.channel).then(|| inbox.clone()))),
            TurnExecutorOptions {
                hosts: Some([channel.to_string()].into_iter().collect()),
                ..Default::default()
            },
        )?;
        let network = Self {
            backend,
            channel,
            room: room.clone(),
            // Kept in the room: computed without the URL's credentials.
            scope: crate::digest::sha256_hex(serde_json::to_vec(&(
                group,
                crate::remote::without_credentials(address)?,
            ))?),
            wake,
            responses: Default::default(),
        };
        let registration: Result<()> = async {
            network.remember_endpoint(kit).await?;
            network.backend.serve(executor).await?;
            Ok(())
        }
        .await;
        if let Err(e) = registration {
            network.close().await;
            return Err(e);
        }
        Ok(network)
    }

    async fn remember_endpoint(&self, kit: &Kit) -> Result<()> {
        // An old address is usable only as the return identity of mail already
        // accepted into this room, on the very same broker and group.
        kit.store()
            .set_private_state(
                &self.room,
                &endpoint(&self.channel),
                Some([("scope".into(), json!(self.scope))].into_iter().collect()),
            )
            .await?;
        kit.store()
            .set_private_state(
                &self.room,
                ADDRESS,
                Some(
                    [("channel".into(), json!(self.channel))]
                        .into_iter()
                        .collect(),
                ),
            )
            .await?;
        Ok(())
    }

    pub async fn owns_endpoint(
        &self,
        kit: &Kit,
        room: &RoomId,
        channel: &ChannelId,
    ) -> Result<bool> {
        if room != &self.room {
            return Ok(false);
        }
        if channel == &self.channel {
            return Ok(true);
        }
        let saved = kit
            .store()
            .get_private_state(room, &endpoint(channel))
            .await?;
        Ok(saved
            .is_some_and(|s| s.get("scope").and_then(|v| v.as_str()) == Some(self.scope.as_str())))
    }

    pub async fn close(&self) {
        let _ = self.backend.close().await;
    }

    pub async fn contacts(&self) -> Result<Vec<(ChannelId, RoomId)>> {
        let mut result = Vec::new();
        for item in self
            .backend
            .executors()
            .await?
            .into_iter()
            .filter(|i| i.online)
        {
            for host in item.hosts.into_iter().flatten() {
                if let Ok(channel) = ChannelId::new(host)
                    && channel != self.channel
                    && let Some(room) = room_of(&channel)
                {
                    result.push((channel, room));
                }
            }
        }
        result.sort();
        result.dedup();
        Ok(result)
    }

    pub async fn request(
        &self,
        kit: &Kit,
        to: &ChannelId,
        id: &EventId,
        mail: &Mail,
        request: Option<AgentRequest>,
    ) -> Result<()> {
        let mut origin = roomkit::model::Metadata::new();
        origin.insert("peer".into(), json!(to));
        if let Some(request) = request {
            origin.insert("request".into(), json!(request));
        }
        // Store the route before transmitting: the answer can beat the acknowledgement.
        kit.store()
            .set_private_state(&self.room, &pending(id), Some(origin))
            .await?;
        self.transmit(&self.channel, to, id, mail).await
    }

    pub async fn agent_request(&self, kit: &Kit, id: &EventId) -> Result<Option<AgentRequest>> {
        let origin = kit
            .store()
            .get_private_state(&self.room, &pending(id))
            .await?;
        Ok(origin
            .and_then(|r| r.get("request").cloned())
            .map(serde_json::from_value)
            .transpose()?)
    }

    async fn transmit(
        &self,
        from: &ChannelId,
        to: &ChannelId,
        id: &EventId,
        mail: &Mail,
    ) -> Result<()> {
        let room = room_of(to).ok_or("Invalid session address")?;
        if mail.from != self.room {
            return Err("Only the advertised session can send here".into());
        }
        let mut call = ChannelCall::new(to.clone(), room, VERB);
        let mut wire = mail.clone();
        wire.remote = Some(Route {
            from: from.clone(),
            to: to.clone(),
        });
        call.arguments = [("id".into(), json!(id)), ("mail".into(), json!(wire))]
            .into_iter()
            .collect();
        let response = self
            .backend
            .control(ChannelControl::ChannelCall(Box::new(call)))
            .await
            .map_err(|_| "Remote session unreachable or acknowledgement lost")?
            .ok_or("Remote session did not acknowledge the message")?;
        if response.error.is_some() || response.result != json!(id) {
            return Err("Remote session refused the message".into());
        }
        Ok(())
    }
}

fn pending(id: &EventId) -> String {
    format!("roomkit_code:outgoing:{id}")
}

fn endpoint(channel: &ChannelId) -> String {
    format!("roomkit_code:mail_endpoint:{channel}")
}
