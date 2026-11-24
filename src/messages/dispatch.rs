//! The receipt for the single inbox record being admitted in each room.
use super::{
    delivery::{Posts, after_post},
    network::Network,
    record::Mail,
};
use roomkit::model::{ChannelId, EventStatus, RoomEvent, TextContent};
use roomkit::{ConversationStore, PostOptions, RoomKit};
use roomkit_sqlite::SqliteStore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

type Kit = RoomKit<SqliteStore>;
const RECEIPT: &str = "roomkit_code:session_mail_dispatch:v1";

#[derive(Serialize, Deserialize)]
pub(super) enum Checkpoint {
    Steering,
    Delivered,
    Post(PendingPost),
}

impl Checkpoint {
    pub(super) async fn save(&self, kit: &Kit, event: &RoomEvent) -> crate::Result<()> {
        let state = [
            ("id".into(), json!(event.id)),
            ("dispatch".into(), serde_json::to_value(self)?),
        ];
        kit.store()
            .set_private_state(&event.room_id, RECEIPT, Some(state.into_iter().collect()))
            .await?;
        Ok(())
    }

    /// Once the steering outcome is known, retain it in memory while storage
    /// recovers. Reading the older claim back would make a known outcome ambiguous.
    pub(super) async fn save_known(
        &self,
        kit: &Kit,
        event: &RoomEvent,
        diagnostics: &crate::diagnostics::Diagnostics,
    ) {
        while let Err(error) = self.save(kit, event).await {
            super::receiver::report_unavailable(diagnostics, error.as_ref()).await;
        }
    }

    pub(super) async fn read(kit: &Kit, event: &RoomEvent) -> crate::Result<Option<Self>> {
        let state = kit
            .store()
            .get_private_state(&event.room_id, RECEIPT)
            .await?;
        let Some(mut state) =
            state.filter(|s| s.get("id").and_then(|id| id.as_str()) == Some(event.id.as_str()))
        else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_value(
            state.remove("dispatch").ok_or("Invalid inbox receipt")?,
        )?))
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PendingPost {
    pub text: String,
    pub agent: ChannelId,
    pub organization: Option<String>,
}

impl PendingPost {
    /// Release the inbox as soon as the engine has recorded the addressed post;
    /// waiting for the model to finish here would prevent steering that turn.
    pub(super) async fn admit(
        self,
        kit: &Kit,
        net: Option<&Network>,
        event: RoomEvent,
        mail: Mail,
        posts: &mut Posts,
    ) -> crate::Result<()> {
        let room = event.room_id.clone();
        let key = format!("dispatch-{}", event.id);
        let host = ChannelId::host();
        let store = kit.store();
        let recorded = || store.get_event_by_idempotency_key(&room, &key, Some(&host));
        if recorded().await?.is_some() {
            return Ok(());
        }
        if !super::writable(kit, &room, self.organization.clone()).await? {
            return after_post(net, kit, &event, &mail, true).await;
        }
        let options = PostOptions {
            organization_id: self.organization,
            addressed_to: vec![self.agent],
            idempotency_key: Some(key.clone()),
            ..Default::default()
        };
        let owner = kit.clone();
        let network = net.cloned();
        let (admitted, mut result) = tokio::sync::oneshot::channel();
        posts.spawn(async move {
            match owner
                .post(&event.room_id, TextContent::new(self.text), options)
                .await
            {
                Ok(posted) => {
                    let _ = admitted.send(Ok(()));
                    after_post(
                        network.as_ref(),
                        &owner,
                        &event,
                        &mail,
                        posted.status == EventStatus::Blocked,
                    )
                    .await
                }
                Err(
                    roomkit::RoomKitError::NoRecipient { .. }
                    | roomkit::RoomKitError::ChannelNotRegistered { .. }
                    | roomkit::RoomKitError::ChannelNotFound { .. }
                    | roomkit::RoomKitError::RoomClosed { .. },
                ) => {
                    after_post(network.as_ref(), &owner, &event, &mail, true).await?;
                    let _ = admitted.send(Ok(()));
                    Ok(())
                }
                Err(error) => {
                    let _ = admitted.send(Err(error));
                    Ok(())
                }
            }
        });
        loop {
            tokio::select! {
                status = &mut result => return Ok(status??),
                () = tokio::time::sleep(Duration::from_millis(50)) => {
                    if recorded().await?.is_some() { return Ok(()) }
                }
            }
        }
    }
}
