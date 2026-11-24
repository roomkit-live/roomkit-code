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

pub(super) async fn report_unavailable(
    diagnostics: &crate::diagnostics::Diagnostics,
    error: &(dyn std::error::Error + Send + Sync),
) {
    diagnostics.record(
        "session_inbox_unavailable",
        json!({"error":error.to_string()}),
        None,
    );
    tokio::time::sleep(Duration::from_secs(5)).await;
}

impl Messages {
    pub(super) async fn receive(
        self: &std::sync::Arc<Self>,
        kit: &Kit,
        room: &RoomId,
        diagnostics: &std::sync::Arc<crate::diagnostics::Diagnostics>,
    ) {
        let mut posts = Posts::new();
        if let Some(net) = self.network.get().filter(|net| net.room == *room).cloned() {
            let (messages, kit) = (self.clone(), kit.clone());
            let diagnostics = diagnostics.clone();
            posts.spawn(async move {
                while let Err(error) = net.reconcile(&kit, &messages).await {
                    report_unavailable(&diagnostics, error.as_ref()).await;
                }
                Ok(())
            });
        }
        loop {
            if let Err(error) = self.poll(kit, room, &mut posts, diagnostics).await {
                report_unavailable(diagnostics, error.as_ref()).await;
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
        let text = format!(
            "Delivery of {} to the active agent could not be confirmed. Check the conversation before resending.",
            event.id
        );
        self.interrupted(kit, &event.room_id, &event.id, &text)
            .await
    }

    pub(super) async fn interrupted(
        &self,
        kit: &Kit,
        room: &RoomId,
        source: &roomkit::model::EventId,
        text: &str,
    ) -> crate::Result<()> {
        use roomkit::model::{ChannelId, EventId};
        let id = EventId::new(format!("uncertain-{source}"))?;
        let stored = if let Some(row) = kit.store().get_event(&id).await? {
            row
        } else {
            crate::system_row::SystemRow {
                room: room.as_str(),
                id: id.as_str(),
                text,
                about: ("session_mail_uncertain", json!(source)),
            }
            .store(kit, &ChannelId::new(crate::application::TERMINAL)?)
            .await?
        };
        self.show(&stored, text.to_owned());
        Ok(())
    }

    async fn poll(
        &self,
        kit: &Kit,
        room: &RoomId,
        posts: &mut Posts,
        diagnostics: &crate::diagnostics::Diagnostics,
    ) -> crate::Result<()> {
        let state = kit.store().get_private_state(room, CURSOR).await?;
        // RoomKit assigns index zero to the first event. None includes that record.
        let mut after = state.and_then(|s| s.get("index")?.as_u64());
        let mut delay = Duration::from_millis(750);
        loop {
            let previous = after;
            if self
                .drain(kit, room, &mut after, posts, diagnostics)
                .await?
            {
                continue;
            }
            delay = idle_delay(delay, previous != after);
            match self.network.get().filter(|net| net.room == *room) {
                Some(net) => tokio::select! {
                    () = net.wake.notified() => {},
                    () = tokio::time::sleep(delay) => {},
                },
                None => tokio::time::sleep(delay).await,
            }
        }
    }

    async fn drain(
        &self,
        kit: &Kit,
        room: &RoomId,
        after: &mut Option<u64>,
        posts: &mut Posts,
        diagnostics: &crate::diagnostics::Diagnostics,
    ) -> crate::Result<bool> {
        while let Some(result) = posts.try_join_next() {
            result??;
        }
        // Sample the durable head *before* querying mail. A simultaneous append
        // beyond this watermark is either returned now or read on the next pass.
        let head = kit.get_room(room, Default::default()).await?.latest_index;
        if head <= *after {
            return Ok(false);
        }
        let records = mail_after(kit, room, *after).await?;
        let full = records.len() == usize::from(BATCH);
        for event in records {
            if posts.len() >= usize::from(BATCH)
                && let Some(result) = posts.join_next().await
            {
                result??;
            }
            let index = event.index;
            if let Some(mail) = Mail::read(&event) {
                self.show(&event, mail.line());
                super::delivery::deliver(self, diagnostics, kit, event, mail, posts).await?;
            }
            save_cursor(kit, room, index).await?;
            *after = Some(index);
        }
        if !full && let Some(index) = head.filter(|i| Some(*i) > *after) {
            save_cursor(kit, room, index).await?;
            *after = Some(index);
        }
        Ok(full)
    }
}

async fn mail_after(
    kit: &Kit,
    room: &RoomId,
    after: Option<u64>,
) -> crate::Result<Vec<roomkit::model::RoomEvent>> {
    Ok(kit
        .store()
        .list_events(
            room,
            ListEventsOptions {
                after_index: after,
                limit: BATCH.into(),
                event_filter: Some(EventFilter {
                    correlation_id: Some(STREAM.into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await?)
}

async fn save_cursor(kit: &Kit, room: &RoomId, index: u64) -> crate::Result<()> {
    kit.store()
        .set_private_state(
            room,
            CURSOR,
            Some([("index".into(), json!(index))].into_iter().collect()),
        )
        .await?;
    Ok(())
}

fn idle_delay(last: Duration, progress: bool) -> Duration {
    if progress {
        Duration::from_millis(750)
    } else {
        (last * 2).min(Duration::from_secs(5))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roomkit::{RoomKitOptions, model::ChannelId};
    use roomkit_sqlite::SqliteStoreOptions;
    #[tokio::test]
    async fn an_idle_inbox_advances_past_unrelated_history_and_backs_off() -> crate::Result<()> {
        let dir = tempfile::tempdir()?;
        let kit = RoomKit::new(RoomKitOptions::with_store(SqliteStore::new(
            SqliteStoreOptions {
                path: dir.path().join("history.sqlite3"),
            },
        )));
        let room = kit.create_room(Default::default()).await?.id;
        let row = crate::system_row::SystemRow {
            room: room.as_str(),
            id: "ordinary",
            text: "Other work",
            about: ("source", json!("test")),
        }
        .store(&kit, &ChannelId::new("terminal")?)
        .await?;
        let messages = Messages::default();
        let diagnostics =
            crate::diagnostics::Diagnostics::open(dir.path().join("diagnostics.jsonl"), false)?;
        let mut after = None;
        let mut posts = Posts::new();
        assert!(
            !messages
                .drain(&kit, &room, &mut after, &mut posts, &diagnostics)
                .await?
        );
        assert_eq!(after, Some(row.index));
        let cursor = kit.store().get_private_state(&room, CURSOR).await?;
        assert_eq!(cursor.and_then(|v| v.get("index")?.as_u64()), after);
        assert_eq!(
            idle_delay(Duration::from_secs(4), false),
            Duration::from_secs(5)
        );
        assert_eq!(
            idle_delay(Duration::from_secs(5), true),
            Duration::from_millis(750)
        );
        kit.close().await?;
        Ok(())
    }
}
