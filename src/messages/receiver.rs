//! Only the interactive session owner delivers an inbox record to its own agent.
use super::{
    Messages,
    delivery::Posts,
    record::{Mail, STREAM},
};
use roomkit::model::RoomId;
use roomkit::{ConversationStore, RoomKit};
use roomkit::{EventFilter, store::ListEventsOptions};
use roomkit_sqlite::SqliteStore;
use serde_json::json;
use std::time::Duration;

const CURSOR: &str = "roomkit_code:session_mail_cursor:v1";
type Kit = RoomKit<SqliteStore>;
const BATCH: u8 = 16;

impl Messages {
    pub(super) async fn receive(
        &self,
        kit: &Kit,
        room: &RoomId,
        diagnostics: &crate::diagnostics::Diagnostics,
    ) {
        let mut posts = Posts::new();
        loop {
            if let Err(error) = self.poll(kit, room, &mut posts).await {
                diagnostics.record(
                    "session_inbox_unavailable",
                    json!({"error":error.to_string()}),
                    None,
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }

    /// RoomKit steering has no idempotency key. Surface an interrupted handover
    /// without injecting the same mail into another turn.
    pub(super) async fn uncertain_steer(
        &self,
        kit: &Kit,
        event: &roomkit::model::RoomEvent,
    ) -> crate::Result<()> {
        use roomkit::model::{ChannelId, EventId};
        let id = EventId::new(format!("uncertain-{}", event.id))?;
        let text = format!(
            "Delivery of {} to the active agent could not be confirmed. Check the conversation before resending.",
            event.id
        );
        if kit.store().get_event(&id).await?.is_none() {
            let row = crate::system_row::SystemRow {
                room: event.room_id.as_str(),
                id: id.as_str(),
                text: &text,
                about: ("session_mail_uncertain", json!(event.id)),
            }
            .event(&ChannelId::new(crate::application::TERMINAL)?)?;
            kit.commit_event(&event.room_id, row, Default::default())
                .await?;
        }
        self.show(&event.room_id, &id, text);
        Ok(())
    }

    async fn poll(&self, kit: &Kit, room: &RoomId, posts: &mut Posts) -> crate::Result<()> {
        let state = kit.store().get_private_state(room, CURSOR).await?;
        // RoomKit assigns index zero to the first event. None includes that record.
        let mut after = state.and_then(|s| s.get("index")?.as_u64());
        loop {
            if !self.drain(kit, room, &mut after, posts).await? {
                tokio::time::sleep(Duration::from_millis(750)).await;
            }
        }
    }

    async fn drain(
        &self,
        kit: &Kit,
        room: &RoomId,
        after: &mut Option<u64>,
        posts: &mut Posts,
    ) -> crate::Result<bool> {
        while let Some(result) = posts.try_join_next() {
            result??;
        }
        let records = kit
            .store()
            .list_events(
                room,
                ListEventsOptions {
                    after_index: *after,
                    limit: BATCH.into(),
                    event_filter: Some(EventFilter {
                        correlation_id: Some(STREAM.into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await?;
        let full = records.len() == usize::from(BATCH);
        for event in records {
            if posts.len() >= usize::from(BATCH)
                && let Some(result) = posts.join_next().await
            {
                result??;
            }
            let index = event.index;
            if let Some(mail) = Mail::read(&event) {
                self.show(room, &event.id, mail.line());
                super::delivery::deliver(self, self.network.get(), kit, event, mail, posts).await?;
            }
            kit.store()
                .set_private_state(
                    room,
                    CURSOR,
                    Some([("index".into(), json!(index))].into_iter().collect()),
                )
                .await?;
            *after = Some(index);
        }
        Ok(full)
    }
}
