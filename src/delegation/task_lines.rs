//! Placement and completion notices shown live and kept in the parent conversation.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use roomkit::RoomKit;
use roomkit::model::ChannelId;
use roomkit_providers_sqlite::SqliteStore;
use roomkit_tui::protocol::{ActorId, Item, Message};
use serde_json::Value;

use super::board::NOTICE_FOR;
use crate::system_row::{self, SystemRow};

/// Where the lines go: the engine and the terminal's channel, once the
/// application installed them; the lines the views show at once.
#[derive(Default)]
pub(super) struct TaskLines {
    kit: OnceLock<(RoomKit<SqliteStore>, ChannelId)>,
    /// Each room's latest lines, and when they came: several tasks can end
    /// within the time a view takes to read them.
    recent: Mutex<HashMap<String, Vec<(Instant, Item)>>>,
}

impl TaskLines {
    pub fn install(&self, kit: &RoomKit<SqliteStore>, terminal: &ChannelId) {
        let _ = self.kit.set((kit.clone(), terminal.clone()));
    }

    /// Record a task notice in its parent room and show it to attached views.
    pub async fn write(&self, room: &str, id: String, about: (&str, Value), text: &str) {
        let terminal = match self.kit.get() {
            Some((kit, terminal)) => {
                let row = SystemRow {
                    room,
                    id: &id,
                    text,
                    about,
                };
                system_row::commit(kit, terminal, row).await;
                terminal.to_string()
            }
            None => crate::names::TERMINAL.to_owned(),
        };
        let line = Item::Message(Message::new(id, ActorId::new(terminal), text));
        let mut recent = crate::sync::lock(&self.recent);
        recent.retain(|_, lines| {
            lines.retain(|(at, _)| at.elapsed() < NOTICE_FOR);
            !lines.is_empty()
        });
        recent
            .entry(room.into())
            .or_default()
            .push((Instant::now(), line));
    }

    /// The lines the views of `room` are shown, while they are recent.
    pub fn recent(&self, room: &str) -> Vec<Item> {
        let recent = crate::sync::lock(&self.recent);
        recent
            .get(room)
            .into_iter()
            .flatten()
            .filter(|(at, _)| at.elapsed() < NOTICE_FOR)
            .map(|(_, line)| line.clone())
            .collect()
    }
}
