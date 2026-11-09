//! The line a compaction leaves in its conversation (HITL a2dfba34): a
//! system row that only the terminal sees, never an agent, stored where the
//! compaction happened, so a resumed conversation shows it in its place; and
//! shown at once to the views watching.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use roomkit::model::{
    ChannelId, ChannelSet, ChannelType, EventId, EventSource, EventType, RoomEvent, RoomId,
    TextContent, Visibility,
};
use roomkit::{CommitEventOptions, RoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{ActorId, Item, Message};
use serde_json::json;

use super::NOTICE_FOR;

/// Where a compaction's line goes: the engine and the terminal's channel,
/// once the application installed them; the lines the views show at once.
#[derive(Default)]
pub(super) struct Journal {
    kit: OnceLock<(RoomKit<SqliteStore>, ChannelId)>,
    /// Each room's latest line, and when it came.
    recent: Mutex<HashMap<String, (Instant, Item)>>,
}

/// One compaction, as its line tells it.
pub(super) struct Told<'a> {
    pub room: &'a str,
    pub agent: &'a str,
    /// The index of the oldest event the agent reads whole after it.
    pub from: u64,
    pub summarized: usize,
    /// The estimated request before and after it.
    pub sizes: (usize, usize),
    pub took: Duration,
    /// Whether its record could not be saved.
    pub unsaved: bool,
}

impl Journal {
    pub fn install(&self, kit: &RoomKit<SqliteStore>, terminal: &ChannelId) {
        let _ = self.kit.set((kit.clone(), terminal.clone()));
    }

    pub fn kit(&self) -> Option<&RoomKit<SqliteStore>> {
        self.kit.get().map(|(kit, _)| kit)
    }

    /// Stores `told`'s line in its room, and shows it to the views.
    pub async fn write(&self, told: &Told<'_>) {
        let text = text(told);
        // A compaction whose record was lost can cut where it did again.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let seed = format!("{}\0{}\0{}\0{nanos}", told.room, told.agent, told.from);
        let id = format!("compaction-{}", &crate::digest::sha256_hex(seed)[..32]);
        let terminal = match self.kit.get() {
            Some((kit, terminal)) => {
                commit(kit, terminal, told, &id, &text).await;
                terminal.to_string()
            }
            None => crate::application::TERMINAL.to_owned(),
        };
        let line = Item::Message(Message::new(id, ActorId::new(terminal), text));
        crate::sync::lock(&self.recent).insert(told.room.into(), (Instant::now(), line));
    }

    /// The line the views of `room` are shown, while it is recent.
    pub fn recent(&self, room: &str) -> Option<Item> {
        crate::sync::lock(&self.recent)
            .get(room)
            .filter(|(at, _)| at.elapsed() < NOTICE_FOR)
            .map(|(_, line)| line.clone())
    }
}

/// Stores `text` as the system row `id` in `told`'s room, from the terminal
/// and for it alone: the agents' histories never hold it. A row that cannot
/// be stored is only shown.
async fn commit(
    kit: &RoomKit<SqliteStore>,
    terminal: &ChannelId,
    told: &Told<'_>,
    id: &str,
    text: &str,
) {
    let (Ok(room), Ok(id), Ok(visibility)) = (
        RoomId::new(told.room),
        EventId::new(id),
        ChannelSet::new([terminal.clone()]).map(Visibility::Channels),
    ) else {
        return;
    };
    let source = EventSource::new(terminal.clone(), ChannelType::WEBSOCKET);
    let mut row = RoomEvent::new(room.clone(), source, TextContent::new(text));
    row.id = id;
    row.r#type = EventType::System;
    row.visibility = visibility;
    row.metadata.insert(
        "compaction".into(),
        json!({"agent": told.agent, "from": told.from, "summarized": told.summarized}),
    );
    let _ = kit
        .commit_event(&room, row, CommitEventOptions::default())
        .await;
}

/// "Conversation compacted · 42 messages summarized · ≈180k → ≈40k tokens · 12 s".
fn text(told: &Told<'_>) -> String {
    let (before, after) = told.sizes;
    let mut text = format!(
        "Conversation compacted · {} messages summarized · ≈{} → ≈{} tokens · {}",
        told.summarized,
        tokens(before),
        tokens(after),
        duration(told.took)
    );
    if told.unsaved {
        text.push_str(" · not saved: a resumed conversation compacts again");
    }
    text
}

/// `850`, `4.2k`, `180k`.
pub(crate) fn tokens(count: usize) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..9_950 => {
            let tenths = (count + 50) / 100;
            format!("{}.{}k", tenths / 10, tenths % 10)
        }
        _ => format!("{}k", (count + 500) / 1_000),
    }
}

/// `12 s`, `1 min 5 s`.
fn duration(took: Duration) -> String {
    let seconds = took.as_secs();
    if seconds < 60 {
        return format!("{seconds} s");
    }
    format!("{} min {} s", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_tells_what_the_compaction_did() {
        let told = Told {
            room: "room",
            agent: "code",
            from: 40,
            summarized: 42,
            sizes: (180_400, 39_600),
            took: Duration::from_millis(12_400),
            unsaved: false,
        };
        assert_eq!(
            text(&told),
            "Conversation compacted · 42 messages summarized · ≈180k → ≈40k tokens · 12 s"
        );
        let unsaved = Told {
            sizes: (4_249, 850),
            took: Duration::from_secs(65),
            unsaved: true,
            ..told
        };
        assert_eq!(
            text(&unsaved),
            "Conversation compacted · 42 messages summarized · ≈4.2k → ≈850 tokens · 1 min 5 s · not saved: a resumed conversation compacts again"
        );
        assert_eq!(tokens(9_949), "9.9k");
        assert_eq!(tokens(9_950), "10k");
    }
}
