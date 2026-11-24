//! Interactive session messages. RoomKit owns storage, addressing and steering.
pub(crate) mod command;
mod delivery;
mod directory;
mod dispatch;
mod network;
mod receiver;
mod record;
mod reply;
#[cfg(test)]
mod tests;
mod tools;

use crate::sync::lock;
use roomkit::model::{EventId, RoomId};
use roomkit::{RoomKit, WeakRoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{ActorId, Event, Item, Message};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(crate) use tools::{LIST_SESSIONS, SEND_MESSAGE};

/// Live items and restored system rows share the same author and message ID.
pub(crate) fn emphasis(message: &Message) -> Option<roomkit_tui::render::Emphasis> {
    let id = message.id.as_str();
    if message.actor.as_str() != crate::application::TERMINAL
        || ![
            "session-msg-",
            "session-reply-session-msg-",
            "sent-session-msg-",
        ]
        .iter()
        .any(|prefix| id.starts_with(prefix))
    {
        return None;
    }
    let mut emphasis = roomkit_tui::render::Emphasis::default();
    emphasis.italic = true;
    Some(emphasis)
}

struct Interactive {
    kit: WeakRoomKit<SqliteStore>,
    group_enabled: bool,
}

#[derive(Default)]
pub(crate) struct Messages {
    interactive: OnceLock<Interactive>,
    network: OnceLock<network::Network>,
    listening: Mutex<HashSet<RoomId>>,
    recent: Mutex<HashMap<RoomId, VecDeque<Item>>>,
    changes: watch::Sender<u64>,
    stop: CancellationToken,
    workers: TaskTracker,
}

impl Messages {
    pub(crate) fn install(&self, kit: &RoomKit<SqliteStore>, group_enabled: bool) {
        let _ = self.interactive.set(Interactive {
            kit: kit.downgrade(),
            group_enabled,
        });
    }

    pub(crate) async fn open_network(
        &self,
        settings: &crate::config::Settings,
        kit: &RoomKit<SqliteStore>,
        room: &RoomId,
        leases: Arc<crate::storage::SessionLeases>,
    ) -> crate::Result<()> {
        let Some(group) = &settings.session_group else {
            return Ok(());
        };
        let address = crate::remote::address(settings.executor_url.as_deref())?;
        let net = network::Network::open(group, &address, kit, room, leases).await?;
        self.network
            .set(net)
            .map_err(|_| "Session network already open".into())
    }

    fn kit(&self) -> Option<RoomKit<SqliteStore>> {
        self.interactive.get()?.kit.upgrade()
    }

    pub(crate) fn listen(
        self: &Arc<Self>,
        room: &RoomId,
        leases: &crate::storage::SessionLeases,
        diagnostics: Arc<crate::diagnostics::Diagnostics>,
    ) {
        let Some(kit) = self.kit() else {
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
        if let Some(net) = self.network.get() {
            net.close().await;
        }
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn show(&self, room: &RoomId, id: &EventId, text: String) {
        let mut recent = lock(&self.recent);
        let items = recent.entry(room.clone()).or_default();
        if items
            .iter()
            .any(|item| matches!(item, Item::Message(m) if m.id.as_str() == id.as_str()))
        {
            return;
        }
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
