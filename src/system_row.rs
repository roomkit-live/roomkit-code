//! A line the application leaves in a conversation: a system row that only
//! the terminal sees, never an agent, stored where it happened so that a
//! resumed conversation shows it in its place.

use roomkit::model::{
    ChannelId, ChannelSet, ChannelType, EventId, EventSource, EventType, RoomEvent, RoomId,
    TextContent, Visibility,
};
use roomkit::{CommitEventOptions, RoomKit};
use roomkit_sqlite::SqliteStore;
use serde_json::Value;

/// One such line.
pub(crate) struct SystemRow<'a> {
    pub room: &'a str,
    pub id: &'a str,
    pub text: &'a str,
    /// The metadata entry that says what the line is about.
    pub about: (&'a str, Value),
}

/// Stores `row` from the terminal and for it alone: the agents' histories
/// never hold it. A row that cannot be stored is only shown.
pub(crate) async fn commit(kit: &RoomKit<SqliteStore>, terminal: &ChannelId, row: SystemRow<'_>) {
    let SystemRow {
        room,
        id,
        text,
        about: (key, value),
    } = row;
    let (Ok(room), Ok(id), Ok(visibility)) = (
        RoomId::new(room),
        EventId::new(id),
        ChannelSet::new([terminal.clone()]).map(Visibility::Channels),
    ) else {
        return;
    };
    let source = EventSource::new(terminal.clone(), ChannelType::WEBSOCKET);
    let mut event = RoomEvent::new(room.clone(), source, TextContent::new(text));
    event.id = id;
    event.r#type = EventType::System;
    event.visibility = visibility;
    event.metadata.insert(key.into(), value);
    let _ = kit
        .commit_event(&room, event, CommitEventOptions::default())
        .await;
}
