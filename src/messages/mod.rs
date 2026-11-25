//! Interactive session messages. RoomKit owns storage, addressing and steering.
pub(crate) mod command;
mod delegated;
mod delivery;
mod worker;
pub(crate) use worker::WorkerReply;
mod directory;
mod dispatch;
mod executor;
mod network;
mod outgoing;
mod receiver;
mod recipients;
mod record;
mod reply;
mod request;
#[cfg(test)]
mod tests;
mod tools;

use crate::sync::lock;
use roomkit::model::{RoomEvent, RoomId};
use roomkit::{RoomKit, WeakRoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{ActorId, Event, Item, Message};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub(crate) use tools::SEND_MESSAGE;

/// Live items and restored system rows share the same author and message ID.
pub(crate) fn emphasis(message: &Message) -> Option<roomkit_tui::render::Emphasis> {
    let id = message.id.as_str();
    if message.actor.as_str() != crate::names::TERMINAL
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

/// The engine on the project's store, as every part of the messaging names
/// it.
type Kit = RoomKit<SqliteStore>;

/// The role a turn reads correspondence in, when it is handed some while
/// it runs: a session's message, a follow-up to a task, a peer's question.
/// One place decides it for them all.
const INJECTED_ROLE: &str = "assistant";

/// The turn correspondence is handed to: `agent`'s in `room`, the one
/// `turn` names or any it runs there.
pub(crate) struct RunningTurn<'a> {
    pub room: &'a RoomId,
    pub agent: &'a roomkit::model::ChannelId,
    pub turn: Option<String>,
    pub organization: Option<String>,
}

/// Hands `content` to the running turn `to` names, and tells how many turns
/// took it: none when it no longer runs.
#[expect(
    clippy::result_large_err,
    reason = "The engine's own error, which each caller reports in its words"
)]
pub(crate) async fn inject(
    kit: &RoomKit<SqliteStore>,
    to: RunningTurn<'_>,
    content: String,
) -> Result<usize, roomkit::RoomKitError> {
    let directive = roomkit::model::InjectMessage {
        role: INJECTED_ROLE.into(),
        content,
    };
    let options = roomkit::SteerOptions {
        organization_id: to.organization,
        turn_id: to.turn,
    };
    kit.steer(to.room, to.agent, directive, options).await
}

struct Interactive {
    kit: WeakRoomKit<SqliteStore>,
    group_enabled: bool,
    pool: Option<std::sync::Weak<crate::remote::pool::Coordinator>>,
}

#[derive(Default)]
pub(crate) struct Messages {
    interactive: OnceLock<Interactive>,
    network: OnceLock<network::Network>,
    /// Set when the session group asked for could not be joined at launch.
    network_unavailable: std::sync::atomic::AtomicBool,
    listening: Mutex<HashSet<RoomId>>,
    recent: Mutex<HashMap<RoomId, VecDeque<Item>>>,
    changes: watch::Sender<u64>,
    stop: CancellationToken,
    workers: TaskTracker,
}

impl Messages {
    pub(crate) fn install(
        &self,
        kit: &RoomKit<SqliteStore>,
        group_enabled: bool,
        pool: Option<&Arc<crate::remote::pool::Coordinator>>,
    ) {
        let _ = self.interactive.set(Interactive {
            kit: kit.downgrade(),
            group_enabled,
            pool: pool.map(Arc::downgrade),
        });
    }

    /// Joins the launch's session group. A group whose address is not
    /// valid, or whose record this project's store does not take, refuses
    /// the launch. One whose broker cannot be reached does not: the
    /// conversation opens without it, its local work unaffected, and says
    /// so; the diagnostics record why, and the session tools refuse with
    /// the reason.
    pub(crate) async fn open_network(
        &self,
        settings: &crate::config::Settings,
        kit: &RoomKit<SqliteStore>,
        room: &RoomId,
        leases: Arc<crate::storage::SessionLeases>,
        diagnostics: &crate::diagnostics::Diagnostics,
    ) -> crate::Result<()> {
        let Some(group) = &settings.session_group else {
            return Ok(());
        };
        let address = crate::remote::address(settings.executor_url.as_deref())?;
        let error = match network::Network::open(group, &address, kit, room, leases).await? {
            network::Joined::Group(net) => {
                return self
                    .network
                    .set(net)
                    .map_err(|_| "Session network already open".into());
            }
            network::Joined::Unreachable(error) => error,
        };
        diagnostics.record(
            "session_group_unavailable",
            serde_json::json!({}),
            Some(&error),
        );
        self.network_unavailable
            .store(true, std::sync::atomic::Ordering::Release);
        // Said in the conversation as it opens, not only when a session
        // tool is first used.
        let text = format!(
            "Session group {group} could not be reached: this conversation runs without it. Relaunch once its Redis is reachable."
        );
        let id = format!(
            "session-group-unavailable-{}",
            uuid::Uuid::new_v4().simple()
        );
        let row = crate::system_row::SystemRow {
            room: room.as_str(),
            id: &id,
            text: &text,
            about: ("session_group_unavailable", serde_json::json!(group)),
        };
        let terminal = roomkit::model::ChannelId::new(crate::names::TERMINAL)?;
        if let Ok(stored) = row.store(kit, &terminal).await {
            self.show(&stored, text);
        }
        Ok(())
    }

    fn pool(&self) -> Option<Arc<crate::remote::pool::Scheduler>> {
        self.interactive.get()?.pool.as_ref()?.upgrade()?.active()
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
                let error = error.to_string();
                let fields = serde_json::json!({});
                diagnostics.record("session_listener_unavailable", fields, Some(&error));
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

    fn show(&self, row: &RoomEvent, text: String) {
        let mut recent = lock(&self.recent);
        let items = recent.entry(row.room_id.clone()).or_default();
        if items
            .iter()
            .any(|item| matches!(item, Item::Message(m) if m.id.as_str() == row.id.as_str()))
        {
            return;
        }
        let mut message = Message::new(row.id.to_string(), ActorId::new("terminal"), text);
        message.position = Some(roomkit_tui::protocol::RowPosition::stored(row.index, None));
        items.push_back(Item::Message(message));
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
        b.channel_id.as_str() == crate::names::TERMINAL
            && b.access == roomkit::model::Access::ReadWrite
    }))
}
