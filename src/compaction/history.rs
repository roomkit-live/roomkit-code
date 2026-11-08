//! An agent's history read outside its turns, for `/compact` to compact at
//! once: what a turn's memory of its reads, the room's latest messages as
//! the agent may know them. As in a turn, the agent's own rows of tool calls
//! and read instructions are left out of them (roomkit-ai
//! `AiChannel::recalled`): the summary reads the messages. What a turn
//! replays of those rows is counted apart, as a turn's footprint counts it.

use roomkit::model::{ChannelId, EventContent, EventType, RoomContext, RoomEvent, RoomId};
use roomkit::store::ListEventsOptions;
use roomkit::{ConversationStore, EventFilter, GetRoomOptions, ListBindingsOptions, RoomKit};
use roomkit_ai::memory::{EstimateEventTokensOptions, estimate_event_tokens};
use roomkit_sqlite::SqliteStore;

use super::WINDOW;

/// How many of the agent's own rows a read takes at a time, as the kit's.
const ROWS_PAGE: u64 = 500;

/// An agent's history outside its turns.
pub(crate) struct History {
    /// The messages, in timeline order.
    pub events: Vec<RoomEvent>,
    /// What a turn would replay of the agent's rounds of tool calls and read
    /// instructions among them, as `(index, turn, tokens)`: a turn
    /// footprint's `replayed`.
    pub replayed: Vec<(u64, Option<String>, usize)>,
}

/// `agent`'s history in `room` from the index `from` on; none when the
/// store cannot be read.
pub(super) async fn read(
    kit: &RoomKit<SqliteStore>,
    room: &str,
    agent: &str,
    from: u64,
) -> Option<History> {
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
    let first = events.first().map_or(from, |event| event.index);
    let replayed = replayed(kit, &room_id, &agent, first).await?;
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
    let events = roomkit::visible_events(&context, &agent);
    Some(History { events, replayed })
}

/// What a turn replays of `agent`'s rows in `room` from `from` on: each
/// ended tool call, its call and its result, and each read instruction.
async fn replayed(
    kit: &RoomKit<SqliteStore>,
    room: &RoomId,
    agent: &ChannelId,
    from: u64,
) -> Option<Vec<(u64, Option<String>, usize)>> {
    let filter = EventFilter {
        event_types: Some(vec![EventType::ToolCallEnd, EventType::InstructionRead]),
        source_channel_id: Some(agent.clone()),
        ..EventFilter::default()
    };
    let mut pieces = Vec::new();
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
        pieces.extend(page.into_iter().map(|row| {
            let tokens = estimate_event_tokens(
                &as_replayed(row.clone()),
                EstimateEventTokensOptions::default(),
            );
            (row.index, row.correlation_id, tokens)
        }));
        if !full {
            return Some(pieces);
        }
    }
}

/// `row` as a turn replays it: a failed call's text once, though its row
/// holds it as its error and as its result, and without the structured copy
/// the views read, which the model never does.
fn as_replayed(mut row: RoomEvent) -> RoomEvent {
    if let EventContent::ToolCall(call) = &mut row.content {
        call.structured_content = None;
        if !call.result.is_null() {
            call.error = None;
        }
    }
    row
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
    use super::*;
    use roomkit::model::{ChannelType, EventSource, ToolCallContent};

    /// A failed call's row holds its text as its error and as its result; a
    /// turn replays it once.
    #[test]
    fn a_failed_call_counts_its_text_once() {
        let text: String = (0..400).map(|word| format!("failure{word} ")).collect();
        let mut call = ToolCallContent::new("bash", "call-1");
        call.result = serde_json::Value::String(text.clone());
        call.error = Some(text);
        let source = EventSource::new(ChannelId::new("code").unwrap(), ChannelType::AI);
        let room = RoomId::new("room").unwrap();
        let row = RoomEvent::new(room, source, EventContent::ToolCall(call));
        let twice = estimate_event_tokens(&row, EstimateEventTokensOptions::default());
        let once = estimate_event_tokens(&as_replayed(row), EstimateEventTokensOptions::default());
        assert!(once * 3 < twice * 2, "{once} of {twice}");
    }
}
