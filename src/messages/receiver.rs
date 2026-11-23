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
            // Steering has no replay key. Claim before delivery: a crash leaves the
            // human-readable record but cannot prompt the model twice.
            let index = event.index;
            kit.store()
                .set_private_state(
                    room,
                    CURSOR,
                    Some([("index".into(), json!(index))].into_iter().collect()),
                )
                .await?;
            *after = Some(index);
            if let Some(mail) = Mail::read(&event) {
                self.show(room, &event.id, mail.line());
                super::delivery::deliver(self.network.get(), kit, event, mail, posts).await?;
            }
        }
        Ok(full)
    }
}
