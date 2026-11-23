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

impl SystemRow<'_> {
    /// Build the terminal-only record, including the metadata the application reads back.
    pub(crate) fn event(self, terminal: &ChannelId) -> crate::Result<RoomEvent> {
        let source = EventSource::new(terminal.clone(), ChannelType::WEBSOCKET);
        let mut event =
            RoomEvent::new(RoomId::new(self.room)?, source, TextContent::new(self.text));
        event.id = EventId::new(self.id)?;
        event.r#type = EventType::System;
        event.visibility = Visibility::Channels(ChannelSet::new([terminal.clone()])?);
        event.metadata.insert(self.about.0.into(), self.about.1);
        Ok(event)
    }
}

/// Stores `row` from the terminal and for it alone: the agents' histories
/// never hold it. A row that cannot be stored is only shown.
pub(crate) async fn commit(kit: &RoomKit<SqliteStore>, terminal: &ChannelId, row: SystemRow<'_>) {
    if let Ok(event) = row.event(terminal) {
        let room = event.room_id.clone();
        let _ = kit
            .commit_event(&room, event, CommitEventOptions::default())
            .await;
    }
}
