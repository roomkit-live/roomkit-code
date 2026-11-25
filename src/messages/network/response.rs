//! Persist the chosen reply or notice before transmitting it. Retrying after
//! an uncertain acknowledgement uses the same payload and RoomKit event ID.
use super::{Kit, Network};
use crate::messages::{
    record::{Kind, Mail, Route},
    reply::{Acknowledgement, response_id},
};
use roomkit::{ConversationStore, model::EventId};
use serde::Deserialize;
use serde_json::json;

/// The response recorded for a message, and whether its peer acknowledged
/// it.
#[derive(Deserialize)]
pub(super) struct Receipt {
    response: Mail,
    pub(super) sent: bool,
}

impl Receipt {
    /// What is still to transmit when `requested` is asked again: the
    /// response recorded, unless its peer already acknowledged it. A reply
    /// that differs from the one recorded is refused; a notice gives way to
    /// it.
    fn replay(self, requested: &Mail) -> crate::Result<Option<Mail>> {
        if self.response != *requested && !matches!(requested.kind, Kind::Notice(_)) {
            return Err("A different response was already recorded for this message".into());
        }
        Ok((!self.sent).then_some(self.response))
    }
}

impl Network {
    pub(in crate::messages) async fn reply(
        &self,
        kit: &Kit,
        route: &Route,
        mail: &Mail,
    ) -> crate::Result<Acknowledgement> {
        if !self.owns_endpoint(kit, &mail.from, &route.to).await? {
            return Err("The receiving session address has expired".into());
        }
        let original = match &mail.kind {
            Kind::Reply(id) | Kind::Notice(id) => id,
            Kind::Message | Kind::ToSession { .. } => return Err("Expected a response".into()),
        };
        let Some(response) = self.reserve_response(kit, original, mail).await? else {
            return Ok(Acknowledgement::Saved);
        };
        self.transmit(
            &route.to,
            &route.from,
            &response_id(&response.kind)?,
            &response,
        )
        .await?;
        Ok(match self.confirm_response(kit, original, response).await {
            Ok(()) => Acknowledgement::Saved,
            Err(_) => Acknowledgement::LocalWritePending,
        })
    }

    async fn reserve_response(
        &self,
        kit: &Kit,
        original: &EventId,
        requested: &Mail,
    ) -> crate::Result<Option<Mail>> {
        // Only this process owns the room. Serialize the read/insert, never
        // the round trip. Concurrent retries are identical at the peer.
        let _guard = self.responses.lock().await;
        if let Some(receipt) = self.receipt(kit, original).await? {
            return receipt.replay(requested);
        }
        let mut entries = self.pending_responses(kit).await?;
        if let Some(value) = entries.get(original.as_str()) {
            let reserved = Receipt {
                response: serde_json::from_value(value.clone())?,
                sent: false,
            };
            return reserved.replay(requested);
        }
        entries.insert(original.to_string(), json!(requested));
        self.save_pending(kit, entries).await?;
        Ok(Some(requested.clone()))
    }

    pub(super) async fn receipt(
        &self,
        kit: &Kit,
        original: &EventId,
    ) -> crate::Result<Option<Receipt>> {
        let stored = kit
            .store()
            .get_private_state(&self.room, &key(original))
            .await?;
        Ok(stored
            .map(|s| serde_json::from_value(s.into()))
            .transpose()?)
    }

    async fn confirm_response(
        &self,
        kit: &Kit,
        original: &EventId,
        response: Mail,
    ) -> crate::Result<()> {
        let _guard = self.responses.lock().await;
        let fields = [
            ("response".into(), json!(response)),
            ("sent".into(), json!(true)),
        ];
        kit.store()
            .set_private_state(
                &self.room,
                &key(original),
                Some(fields.into_iter().collect()),
            )
            .await?;
        self.remove_pending(kit, original).await
    }
}

fn key(original: &EventId) -> String {
    format!("roomkit_code:session_reply:{original}")
}
