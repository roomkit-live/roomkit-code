//! A named interactive session advertises one mailbox using RoomKit channel calls.
//! The broker carries mail between independent databases. Each owner delivers locally.
mod channel;
use super::record::{Kind, Mail, Route};
use crate::{Result, storage::SessionLeases};
use roomkit::model::{ChannelId, EventId, RoomId};
use roomkit::turns::{ChannelCall, TurnBackend, wire::ChannelControl};
use roomkit::{
    ConversationStore, RedisTurnBackend, RedisTurnBackendOptions, RoomKit, TurnExecutor,
    TurnExecutorOptions,
};
use roomkit_sqlite::SqliteStore;
use serde_json::json;
use std::sync::Arc;

type Kit = RoomKit<SqliteStore>;
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
        let inbox = channel::Inbox::new(kit.downgrade(), room.clone(), channel.clone(), leases);
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
        };
        let registration: Result<()> = async {
            kit.store()
                .set_private_state(
                    room,
                    ADDRESS,
                    Some(
                        [("channel".into(), json!(network.channel))]
                            .into_iter()
                            .collect(),
                    ),
                )
                .await?;
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

    pub async fn reply(&self, kit: &Kit, route: &Route, id: &EventId, mail: &Mail) -> Result<()> {
        if self.channel != route.to {
            return Err("The receiving session address has expired".into());
        }
        self.send(kit, &route.from, id, mail).await?;
        if let Kind::Reply(original) = &mail.kind {
            kit.store()
                .set_private_state(
                    &self.room,
                    &sent(original),
                    Some([("sent".into(), json!(true))].into_iter().collect()),
                )
                .await?;
        }
        Ok(())
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

    pub async fn send(&self, kit: &Kit, to: &ChannelId, id: &EventId, mail: &Mail) -> Result<()> {
        let room = room_of(to).ok_or("Invalid session address")?;
        if mail.from != self.room {
            return Err("Only the advertised session can send here".into());
        }
        if mail.kind == Kind::Message {
            // A return is accepted only from this endpoint for this request.
            kit.store()
                .set_private_state(
                    &self.room,
                    &pending(id),
                    Some([("peer".into(), json!(to))].into_iter().collect()),
                )
                .await?;
        }
        let mut call = ChannelCall::new(to.clone(), room, VERB);
        let mut wire = mail.clone();
        wire.remote = Some(Route {
            from: self.channel.clone(),
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

pub(super) fn sent(id: &EventId) -> String {
    format!("roomkit_code:session_reply:{id}")
}
