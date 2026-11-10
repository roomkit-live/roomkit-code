//! The agent a conversation designates: the one that receives what the
//! person writes there.
//!
//! It is the active agent of the room's conversation state (RFC §19.2),
//! which the engine keeps in the room's metadata and its orchestration
//! strategies read and write. A strategy installed on the room later finds
//! the agent where it looks for it, beside what it stores of itself there,
//! as a discussion stores its participants.
//!
//! A view shows the agent its conversation designates, whatever agent was
//! selected last elsewhere: a conversation's history only reaches an agent,
//! and its provider, selected in that conversation.

use roomkit::model::{ChannelId, ChannelType, RoomId};
use roomkit::orchestration::{TransitionOptions, get_conversation_state, save_conversation_state};
use roomkit::store::ListEventsOptions;
use roomkit::{ConversationStore, EventFilter, RoomKit, RoomKitError};
use roomkit_sqlite::SqliteStore;

/// Why a conversation's agent changed, as its state records it.
const SELECTED: &str = "selected in roomkit-code";

/// The agent conversation `room` designates. A conversation stored before
/// its agent was recorded designates the agent that worked in it last. None
/// for one nobody worked in yet, or that cannot be read.
pub(crate) async fn designated(kit: &RoomKit<SqliteStore>, room: &RoomId) -> Option<ChannelId> {
    let stored = kit.store().get_room(room).await.ok()??;
    let recorded = get_conversation_state(&stored)
        .ok()
        .and_then(|state| state.active_agent_id);
    match recorded {
        Some(agent) => Some(agent),
        None => last_at_work(kit, room).await,
    }
}

/// The agent the latest of `room`'s events comes from.
async fn last_at_work(kit: &RoomKit<SqliteStore>, room: &RoomId) -> Option<ChannelId> {
    let latest = ListEventsOptions {
        limit: 1,
        newest_first: true,
        event_filter: Some(EventFilter {
            source_channel_type: Some(ChannelType::AI),
            ..EventFilter::default()
        }),
        ..ListEventsOptions::default()
    };
    let events = kit.store().list_events(room, latest).await.ok()?;
    events
        .into_iter()
        .next()
        .map(|event| event.source.channel_id)
}

/// Records `agent` as the one conversation `room` designates. The first
/// agent is set as a strategy sets its own when it is installed on a room;
/// a change is a transition of the conversation's state, which keeps the
/// agent before it and when it changed.
///
/// # Errors
///
/// A room the store lacks, a stored state that is not one, and what the
/// store fails with.
pub(crate) async fn designate(
    kit: &RoomKit<SqliteStore>,
    room: &RoomId,
    agent: &ChannelId,
) -> crate::Result<()> {
    let store = kit.store();
    let stored = store.get_room(room).await?;
    let stored = stored.ok_or_else(|| RoomKitError::RoomNotFound(room.clone()))?;
    let state = get_conversation_state(&stored)?;
    let next = match &state.active_agent_id {
        Some(active) if active == agent => return Ok(()),
        Some(_) => {
            let change = TransitionOptions {
                to_agent: Some(agent.clone()),
                reason: SELECTED.into(),
                ..TransitionOptions::default()
            };
            state.transition(state.phase.clone(), change)
        }
        None => {
            let mut first = state;
            first.active_agent_id = Some(agent.clone());
            first
        }
    };
    save_conversation_state(store, room, &next).await?;
    Ok(())
}
