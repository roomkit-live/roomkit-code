//! Only unconfirmed responses need reconciliation when a session rejoins.
use super::{Kit, Network};
use crate::messages::{
    Messages,
    record::{Kind, Mail},
    reply::Acknowledgement,
};
use roomkit::{
    ConversationStore,
    model::{EventId, Metadata},
};

const PENDING: &str = "roomkit_code:pending_session_responses:v1";

impl Network {
    // The caller holds the response lock while changing the room's pending set.
    pub(super) async fn remove_pending(&self, kit: &Kit, id: &EventId) -> crate::Result<()> {
        let mut entries = self.pending_responses(kit).await?;
        entries.remove(id.as_str());
        self.save_pending(kit, entries).await
    }

    pub(super) async fn save_pending(&self, kit: &Kit, entries: Metadata) -> crate::Result<()> {
        kit.store()
            .set_private_state(
                &self.room,
                PENDING,
                (!entries.is_empty()).then_some(entries),
            )
            .await?;
        Ok(())
    }

    pub(super) async fn pending_responses(&self, kit: &Kit) -> crate::Result<Metadata> {
        Ok(kit
            .store()
            .get_private_state(&self.room, PENDING)
            .await?
            .unwrap_or_default())
    }

    pub(in crate::messages) async fn reconcile(
        &self,
        kit: &Kit,
        messages: &Messages,
    ) -> crate::Result<()> {
        let pending = self.pending_responses(kit).await?;
        for (raw, value) in pending {
            let id = EventId::new(raw)?;
            if let Err(error) = self.retry_response(kit, &id, value).await {
                let text = format!("Could not confirm the response to {id} after restart: {error}");
                messages.interrupted(kit, &self.room, &id, &text).await?;
            }
        }
        Ok(())
    }

    async fn retry_response(
        &self,
        kit: &Kit,
        id: &EventId,
        value: serde_json::Value,
    ) -> crate::Result<()> {
        if self.already_confirmed(kit, id).await? {
            return Ok(());
        }
        let room = kit.get_room(&self.room, Default::default()).await?;
        if room.organization_id.is_some()
            || !crate::messages::writable(kit, &self.room, None).await?
        {
            return Err("The original session no longer admits network correspondence".into());
        }
        let response: Mail = serde_json::from_value(value)?;
        if !matches!(&response.kind, Kind::Reply(original) | Kind::Notice(original) if original == id)
        {
            return Err("Invalid pending response".into());
        }
        let (_, original) =
            crate::messages::reply::original(kit, &self.room, id.to_string()).await?;
        let route = original.remote.ok_or("Missing original return route")?;
        match self.reply(kit, &route, &response).await? {
            Acknowledgement::Saved => Ok(()),
            Acknowledgement::LocalWritePending => {
                Err("The peer accepted the response but the local receipt is pending".into())
            }
        }
    }

    async fn already_confirmed(&self, kit: &Kit, id: &EventId) -> crate::Result<bool> {
        let _guard = self.responses.lock().await;
        if self.receipt(kit, id).await?.is_some_and(|r| r.sent) {
            // Confirmation reached disk just before the process stopped.
            self.remove_pending(kit, id).await?;
            return Ok(true);
        }
        Ok(false)
    }
}
