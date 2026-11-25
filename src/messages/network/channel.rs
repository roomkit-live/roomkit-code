//! The exported channel accepts one verb and can write only to its bound room.
use super::{Kit, VERB, digest, pending, room_of};
use crate::{
    Result,
    messages::record::{Kind, Mail, check_text},
    storage::SessionLeases,
};
use roomkit::{Channel, ChannelError, ConversationStore, WeakRoomKit, model::*};
use roomkit_sqlite::SqliteStore;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct Inbox {
    kit: WeakRoomKit<SqliteStore>,
    room: RoomId,
    pub channel: ChannelId,
    leases: Arc<SessionLeases>,
    wake: Arc<tokio::sync::Notify>,
    /// The group's key: with one, only what it sealed is accepted.
    key: Option<super::seal::Key>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    id: EventId,
    /// As it was sent: its seal is checked on it before it is read.
    mail: Value,
    #[serde(default)]
    seal: Option<String>,
}

impl Inbox {
    pub fn new(
        kit: WeakRoomKit<SqliteStore>,
        room: RoomId,
        channel: ChannelId,
        leases: Arc<SessionLeases>,
        wake: Arc<tokio::sync::Notify>,
        key: Option<super::seal::Key>,
    ) -> Self {
        Self {
            kit,
            room,
            channel,
            leases,
            wake,
            key,
        }
    }
    async fn accept(&self, room: &RoomId, verb: &str, arguments: Metadata) -> Result<Value> {
        if *room != self.room || verb != VERB || !self.leases.accepts_messages(room.as_str())? {
            return Err("This mailbox is unavailable".into());
        }
        let kit = self.kit.upgrade().ok_or("Session closed")?;
        let target = kit.get_room(room, Default::default()).await?;
        if target.organization_id.is_some() || !crate::messages::writable(&kit, room, None).await? {
            return Err("Session does not accept mail".into());
        }
        let Envelope { id, mail, seal } = serde_json::from_value(arguments.into())?;
        // Before anything in the mail is believed, its sender included.
        super::seal::check(self.key.as_ref(), &id, &mail, seal.as_deref())?;
        let mail: Mail = serde_json::from_value(mail)?;
        validate(&kit, room, &self.channel, &id, &mail).await?;
        mail.write(&kit, room, id.clone(), None).await?;
        self.wake.notify_one();
        Ok(json!(id))
    }
}

async fn validate(
    kit: &Kit,
    room: &RoomId,
    channel: &ChannelId,
    id: &EventId,
    mail: &Mail,
) -> Result<()> {
    // Notices include a short quote of the original. Bound all incoming payloads.
    check_text(&mail.text)?;
    let route = mail.remote.as_ref().ok_or("Missing return address")?;
    let from = &route.from;
    if &route.to != channel || from == channel || room_of(from).as_ref() != Some(&mail.from) {
        return Err("Invalid return address".into());
    }
    let (original, prefix) = match &mail.kind {
        Kind::Message if valid_request(id) => return Ok(()),
        Kind::Message | Kind::ToSession { .. } => return Err("Invalid network message".into()),
        Kind::Reply(original) => (original, "session-reply-"),
        Kind::Notice(original) => (original, "session-notice-"),
    };
    if id.as_str() != format!("{prefix}{original}") || !valid_request(original) {
        return Err("Invalid response ID".into());
    }
    let route = kit
        .store()
        .get_private_state(room, &pending(original))
        .await?;
    if !route.is_some_and(|r| r.get("peer") == Some(&json!(from))) {
        return Err("Unknown correspondence".into());
    }
    Ok(())
}

fn valid_request(id: &EventId) -> bool {
    id.as_str()
        .strip_prefix("session-msg-")
        .is_some_and(|s| digest(s, 64))
}

impl Channel for Inbox {
    fn channel_id(&self) -> ChannelId {
        self.channel.clone()
    }
    fn channel_type(&self) -> ChannelType {
        ChannelType::CLI
    }
    async fn handle_inbound(&self, _: InboundMessage, _: RoomContext) -> Result<RoomEvent> {
        Err("Use the session messaging verb".into())
    }
    async fn deliver(
        &self,
        _: RoomEvent,
        _: ChannelBinding,
        _: RoomContext,
    ) -> Result<ChannelOutput> {
        Err("This mailbox does not serve agent turns".into())
    }
    async fn channel_call(
        &self,
        room: RoomId,
        verb: String,
        arguments: Metadata,
    ) -> std::result::Result<Value, ChannelError> {
        self.accept(&room, &verb, arguments).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::record::Route;
    use roomkit::{RoomKit, RoomKitOptions};
    use roomkit_sqlite::SqliteStoreOptions;

    #[tokio::test]
    async fn returned_mail_requires_the_expected_live_endpoints_and_request() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = SqliteStore::new(SqliteStoreOptions {
            path: dir.path().join("history.sqlite3"),
        });
        let kit = RoomKit::new(RoomKitOptions::with_store(store));
        let room = kit.create_room(Default::default()).await?.id;
        let original = EventId::new(format!("session-msg-{}", "0".repeat(64)))?;
        let id = crate::messages::reply::reply_id(&original)?;
        let endpoint =
            |name: &str| ChannelId::new(format!("session-mail-{}-{name}", "a".repeat(32)));
        let peer = endpoint("review")?;
        let channel = endpoint(room.as_str())?;
        let mail = Mail {
            from: RoomId::new("review")?,
            remote: Some(Route {
                from: peer.clone(),
                to: channel.clone(),
            }),
            text: "Reviewed".into(),
            kind: Kind::Reply(original.clone()),
        };
        assert!(validate(&kit, &room, &channel, &id, &mail).await.is_err());
        kit.store()
            .set_private_state(
                &room,
                &pending(&original),
                Some([("peer".into(), json!(peer))].into_iter().collect()),
            )
            .await?;
        validate(&kit, &room, &channel, &id, &mail).await?;
        assert!(
            validate(&kit, &room, &endpoint("expired")?, &id, &mail)
                .await
                .is_err()
        );
        assert!(
            validate(&kit, &room, &channel, &original, &mail)
                .await
                .is_err()
        );
        let other = Mail {
            from: RoomId::new("other")?,
            remote: Some(Route {
                from: endpoint("other")?,
                to: channel.clone(),
            }),
            ..mail
        };
        assert!(validate(&kit, &room, &channel, &id, &other).await.is_err());
        kit.close().await?;
        Ok(())
    }
}
