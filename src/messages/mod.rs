//! Project-local session messages. RoomKit owns storage, addressing and steering.
pub(crate) mod command;
mod receiver;
mod record;
mod reply;
#[cfg(test)]
mod tests;

use crate::sync::lock;
use roomkit::model::{EventId, RoomId};
use roomkit::{RoomKit, WeakRoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{ActorId, Event, Item, Message};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(crate) use reply::{NAME as SEND_MESSAGE, Reply};

#[derive(Default)]
pub(crate) struct Messages {
    kit: OnceLock<WeakRoomKit<SqliteStore>>,
    listening: Mutex<HashSet<RoomId>>,
    recent: Mutex<HashMap<RoomId, VecDeque<Item>>>,
    changes: watch::Sender<u64>,
    stop: CancellationToken,
    workers: TaskTracker,
}

impl Messages {
    pub(crate) fn install(&self, kit: &RoomKit<SqliteStore>) {
        let _ = self.kit.set(kit.downgrade());
    }

    pub(crate) fn enabled(&self) -> bool {
        self.kit.get().is_some()
    }

    pub(crate) fn listen(
        self: &Arc<Self>,
        room: &RoomId,
        leases: &crate::storage::SessionLeases,
        diagnostics: Arc<crate::diagnostics::Diagnostics>,
    ) {
        let Some(kit) = self.kit.get().and_then(WeakRoomKit::upgrade) else {
            return;
        };
        if self.stop.is_cancelled() || !lock(&self.listening).insert(room.clone()) {
            return;
        }
        let lease = match leases.listen(room.as_str()) {
            Ok(lease) => lease,
            Err(error) => {
                lock(&self.listening).remove(room);
                diagnostics.record(
                    "session_listener_unavailable",
                    serde_json::json!({"error":error.to_string()}),
                    None,
                );
                return;
            }
        };
        let this = self.clone();
        let room = room.clone();
        self.workers.spawn(async move {
            let _lease = lease;
            tokio::select! {
                () = this.stop.cancelled() => (),
                () = this.receive(&kit, &room, &diagnostics) => (),
            }
        });
    }

    pub(crate) async fn close(&self) {
        self.stop.cancel();
        self.workers.close();
        self.workers.wait().await;
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn show(&self, room: &RoomId, id: &EventId, text: String) {
        let mut recent = lock(&self.recent);
        let items = recent.entry(room.clone()).or_default();
        items.push_back(Item::Message(Message::new(
            id.to_string(),
            ActorId::new("terminal"),
            text,
        )));
        if items.len() > 16 {
            items.pop_front();
        }
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    pub(crate) fn events(&self, room: &RoomId) -> Vec<Event> {
        lock(&self.recent)
            .get(room)
            .into_iter()
            .flatten()
            .cloned()
            .map(|item| Event::Item {
                conversation: room.as_str().into(),
                item,
            })
            .collect()
    }
}

/// Both endpoints must still admit the terminal when they handle a message.
async fn writable(
    kit: &RoomKit<SqliteStore>,
    room: &RoomId,
    organization_id: Option<String>,
) -> Result<bool, String> {
    let bindings = kit
        .list_bindings(room, roomkit::ListBindingsOptions { organization_id })
        .await
        .map_err(|_| "Cannot inspect session access")?;
    Ok(bindings.iter().any(|b| {
        b.channel_id.as_str() == crate::application::TERMINAL
            && b.access == roomkit::model::Access::ReadWrite
    }))
}
