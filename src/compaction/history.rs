//! An agent's history read outside its turns, for `/compact` to compact at
//! once: what a turn of its would read, as the kit reads it (roomkit
//! `history::read`): the room's latest messages and the agent's own rows a
//! turn replays, its tool calls and the instructions its turns read, as the
//! agent may know them.

use roomkit::model::{ChannelId, EventType, RoomContext, RoomEvent, RoomId};
use roomkit::store::ListEventsOptions;
use roomkit::{ConversationStore, EventFilter, GetRoomOptions, ListBindingsOptions, RoomKit};
use roomkit_sqlite::SqliteStore;

use super::WINDOW;

/// How many of the agent's own rows a read takes at a time, as the kit's.
const ROWS_PAGE: u64 = 500;

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
    let first = events.iter().map(|event| event.index).min().unwrap_or(0);
    events.extend(own_rows(kit, &room_id, &agent, first.max(from)).await?);
    events.retain(|event| event.index >= from);
    events.sort_by_key(|event| event.index);
    events.dedup_by(|later, earlier| later.id == earlier.id);
    let mut context = RoomContext::new(
        kit.get_room(&room_id, GetRoomOptions::default())
            .await
            .ok()?,
    );
    context.bindings = kit
        .list_bindings(&room_id, ListBindingsOptions::default())
        .await
        .ok()?;
    context.recent_events = events;
    Some(roomkit::visible_events(&context, &agent))
}

/// `agent`'s tool calls and read instructions in `room` from `from` on.
async fn own_rows(
    kit: &RoomKit<SqliteStore>,
    room: &RoomId,
    agent: &ChannelId,
    from: u64,
) -> Option<Vec<RoomEvent>> {
    let filter = EventFilter {
        event_types: Some(vec![
            EventType::ToolCallStart,
            EventType::ToolCallEnd,
            EventType::InstructionRead,
        ]),
        source_channel_id: Some(agent.clone()),
        ..EventFilter::default()
    };
    let mut rows = Vec::new();
    let mut after = from.checked_sub(1);
    loop {
        let page = ListEventsOptions {
            limit: ROWS_PAGE,
            event_filter: Some(filter.clone()),
            after_index: after,
            ..ListEventsOptions::default()
        };
        let page = kit.store().list_events(room, page).await.ok()?;
        let full = u64::try_from(page.len()).is_ok_and(|read| read >= ROWS_PAGE);
        after = page.last().map(|last| last.index).or(after);
        rows.extend(page);
        if !full {
            return Some(rows);
        }
    }
}
