//! An agent's history read outside its turns, for `/compact` to compact at
//! once: what a turn's memory of its reads, the room's latest messages as
//! the agent may know them. As in a turn, the agent's own rows of tool calls
//! and read instructions are left out (roomkit-ai `AiChannel::recalled`):
//! the summary reads the messages.

use roomkit::model::{ChannelId, EventType, RoomContext, RoomEvent, RoomId};
use roomkit::store::ListEventsOptions;
use roomkit::{ConversationStore, EventFilter, GetRoomOptions, ListBindingsOptions, RoomKit};
use roomkit_sqlite::SqliteStore;

use super::WINDOW;

/// `agent`'s history in `room` from the index `from` on, in timeline order;
/// none when the store cannot be read.
pub(super) async fn read(
    kit: &RoomKit<SqliteStore>,
    room: &str,
    agent: &str,
    from: u64,
) -> Option<Vec<RoomEvent>> {
    let (room_id, agent) = (RoomId::new(room).ok()?, ChannelId::new(agent).ok()?);
    let messages = ListEventsOptions {
        limit: u64::try_from(WINDOW).unwrap_or(u64::MAX),
        newest_first: true,
        event_filter: Some(EventFilter {
            event_types: Some(vec![EventType::Message]),
            include_blocked: true,
            ..EventFilter::default()
        }),
        ..ListEventsOptions::default()
    };
    let mut events = kit.store().list_events(&room_id, messages).await.ok()?;
    events.retain(|event| event.index >= from);
    events.sort_by_key(|event| event.index);
    let room = kit
        .get_room(&room_id, GetRoomOptions::default())
        .await
        .ok()?;
    let mut context = RoomContext::new(room);
    context.bindings = kit
        .list_bindings(&room_id, ListBindingsOptions::default())
        .await
        .ok()?;
    context.recent_events = events;
    Some(roomkit::visible_events(&context, &agent))
}
