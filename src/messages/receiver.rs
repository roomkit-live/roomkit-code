//! Only the interactive session owner delivers an inbox record to its own agent.
use super::{
    Messages,
    cursor::Cursor,
    delivery::Posts,
    record::{Mail, STREAM},
};
use roomkit::ConversationStore;
use roomkit::model::RoomId;
use roomkit::{EventFilter, store::ListEventsOptions};
use serde_json::json;
use std::time::Duration;

use super::Kit;
const BATCH: u8 = 16;

pub(super) async fn report_unavailable(
    diagnostics: &crate::diagnostics::Diagnostics,
    error: &(dyn std::error::Error + Send + Sync),
) {
    // Classified, never its text: a broker's error can hold its URL, and
    // the password in it.
    let error = error.to_string();
    diagnostics.record("session_inbox_unavailable", json!({}), Some(&error));
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
        while let Err(error) = recover_session_questions(kit, room).await {
            report_unavailable(diagnostics, error.as_ref()).await;
        }
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
            .store(kit, &ChannelId::new(crate::names::TERMINAL)?)
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
        diagnostics: &std::sync::Arc<crate::diagnostics::Diagnostics>,
    ) -> crate::Result<()> {
        let local_only = self
            .network_unavailable
            .load(std::sync::atomic::Ordering::Acquire);
        let mut cursor = Cursor::load(kit, room, local_only).await?;
        let mut delay = Duration::from_millis(750);
        loop {
            let previous = cursor.after();
            if self
                .drain(kit, room, &mut cursor, posts, diagnostics)
                .await?
            {
                continue;
            }
            delay = idle_delay(delay, previous != cursor.after());
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
        cursor: &mut Cursor,
        posts: &mut Posts,
        diagnostics: &std::sync::Arc<crate::diagnostics::Diagnostics>,
    ) -> crate::Result<bool> {
        while let Some(result) = posts.try_join_next() {
            result??;
        }
        // Sample the durable head *before* querying mail. A simultaneous append
        // beyond this watermark is either returned now or read on the next pass.
        let head = kit.get_room(room, Default::default()).await?.latest_index;
        if head <= cursor.after() {
            return Ok(false);
        }
        let records = mail_after(kit, room, cursor.after()).await?;
        let full = records.len() == usize::from(BATCH);
        for event in records {
            if posts.len() >= usize::from(BATCH)
                && let Some(result) = posts.join_next().await
            {
                result??;
            }
            let index = event.index;
            if let Some(mail) = Mail::read(&event)
                && cursor.pending(mail.remote.is_some(), index)
            {
                self.show(&event, mail.line());
                super::delivery::deliver(self, diagnostics, kit, event, mail, posts).await?;
            }
            cursor.advance(kit, room, index).await?;
        }
        if !full && let Some(index) = head.filter(|i| Some(*i) > cursor.after()) {
            cursor.advance(kit, room, index).await?;
        }
        Ok(full)
    }
}

/// Only a previous owner could have advanced this cursor or left an admitted
/// dispatch. Its turns have stopped. Never repeat those posts after restart.
async fn recover_session_questions(kit: &Kit, room: &RoomId) -> crate::Result<()> {
    let through = Cursor::load(kit, room, true).await?.after();
    let mut after = None;
    loop {
        let rows = mail_after(kit, room, after).await?;
        for row in &rows {
            let past = Some(row.index) <= through;
            if let Some(mail) =
                Mail::read(row).filter(|m| matches!(m.kind, super::record::Kind::ToSession { .. }))
                && (past || previously_admitted(kit, row, &mail).await?)
                && !answered(kit, row).await?
            {
                super::delivery::notice(
                    None,
                    kit,
                    row,
                    &mail,
                    "The session owner stopped before a reply was confirmed.",
                )
                .await?;
            }
            if !past {
                return Ok(());
            }
            after = Some(row.index);
        }
        if rows.len() < usize::from(BATCH) {
            return Ok(());
        }
    }
}

pub(super) async fn answered(kit: &Kit, row: &roomkit::model::RoomEvent) -> crate::Result<bool> {
    let terminal = roomkit::model::ChannelId::new(crate::names::TERMINAL)?;
    let key = format!("session-outcome-{}", row.id);
    Ok(kit
        .store()
        .get_event_by_idempotency_key(&row.room_id, &key, Some(&terminal))
        .await?
        .is_some())
}

async fn previously_admitted(
    kit: &Kit,
    row: &roomkit::model::RoomEvent,
    mail: &Mail,
) -> crate::Result<bool> {
    use super::dispatch::Checkpoint;
    match Checkpoint::read(kit, row).await? {
        Some(Checkpoint::Post(_)) => {
            let key = format!("dispatch-{}", row.id);
            let host = roomkit::model::ChannelId::host();
            Ok(kit
                .store()
                .get_event_by_idempotency_key(mail.destination(&row.room_id), &key, Some(&host))
                .await?
                .is_some())
        }
        Some(Checkpoint::Steering | Checkpoint::Delivered) => Ok(true),
        None => Ok(false),
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

fn idle_delay(last: Duration, progress: bool) -> Duration {
    if progress {
        Duration::from_millis(750)
    } else {
        (last * 2).min(Duration::from_secs(5))
    }
}

#[cfg(test)]
mod tests;
