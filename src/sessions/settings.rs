//! What a conversation keeps of its own settings, beside the agent it
//! designates (`agent`): the mode it was left in, and the model and the
//! effort each of its agents runs with there. Nothing done in another
//! conversation, another project or another instance changes them.
//!
//! The record is the application's, in the room's private state, with one
//! entry for each agent: it holds with several agents in a room. The
//! designated agent is the engine's to keep, where its orchestration reads
//! it.

use std::collections::BTreeMap;

use roomkit::model::{ChannelId, ChannelType, EventType, RoomId};
use roomkit::store::ListEventsOptions;
use roomkit::{ConversationStore, EventFilter, RoomKit};
use roomkit_sqlite::SqliteStore;
use serde::{Deserialize, Serialize};

use crate::permissions::{Mode, Policy};

const STATE_KEY: &str = "roomkit_code:settings";

/// One change of a conversation's record at a time: it is read, then
/// written, and a mode kept as it changes must not undo an agent's model
/// kept meanwhile.
static CHANGING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The model and the effort an agent runs with in a conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Running {
    pub model: String,
    /// The provider's default when none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// One conversation's settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Kept {
    /// The mode it was left in, by its name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// What each agent that worked in it runs with there, by agent.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agents: BTreeMap<String, Running>,
}

/// What `room` keeps: nothing for a room that keeps none, or whose record
/// cannot be read.
pub(crate) async fn kept(kit: &RoomKit<SqliteStore>, room: &RoomId) -> Kept {
    read(kit, room).await.unwrap_or_default()
}

async fn read(kit: &RoomKit<SqliteStore>, room: &RoomId) -> crate::Result<Kept> {
    let stored = kit.store().get_private_state(room, STATE_KEY).await?;
    // A record that is not one is as none: the next change writes it anew.
    Ok(stored
        .and_then(|state| serde_json::from_value(serde_json::Value::Object(state)).ok())
        .unwrap_or_default())
}

/// Changes what `room` keeps. Nothing is written when `change` changes
/// nothing.
///
/// # Errors
///
/// What the store fails with: a record that cannot be read is not written
/// over.
pub(crate) async fn keep(
    kit: &RoomKit<SqliteStore>,
    room: &RoomId,
    change: impl FnOnce(&mut Kept),
) -> crate::Result<()> {
    let _changing = CHANGING.lock().await;
    let before = read(kit, room).await?;
    let mut after = before.clone();
    change(&mut after);
    if after == before {
        return Ok(());
    }
    let serde_json::Value::Object(state) = serde_json::to_value(&after)? else {
        return Err("A conversation's settings are not a record".into());
    };
    kit.store()
        .set_private_state(room, STATE_KEY, Some(state))
        .await?;
    Ok(())
}

/// Forgets what `room` keeps: a draft nothing was sent in starts anew.
///
/// # Errors
///
/// What the store fails with.
pub(crate) async fn forget(kit: &RoomKit<SqliteStore>, room: &RoomId) -> crate::Result<()> {
    let _changing = CHANGING.lock().await;
    kit.store().set_private_state(room, STATE_KEY, None).await?;
    Ok(())
}

/// The provider and the model of `agent`'s latest answer in `room`, as the
/// engine noted them on it: what a conversation stored before it kept its
/// settings tells of the model it ran on.
pub(crate) async fn last_model(
    kit: &RoomKit<SqliteStore>,
    room: &RoomId,
    agent: &ChannelId,
) -> Option<(String, String)> {
    let latest = ListEventsOptions {
        limit: 1,
        newest_first: true,
        event_filter: Some(EventFilter {
            event_types: Some(vec![EventType::Message]),
            source_channel_id: Some(agent.clone()),
            source_channel_type: Some(ChannelType::AI),
            ..EventFilter::default()
        }),
        ..ListEventsOptions::default()
    };
    let answers = kit.store().list_events(room, latest).await.ok()?;
    let answer = answers.into_iter().next()?;
    let noted = |key: &str| Some(answer.metadata.get(key)?.as_str()?.to_owned());
    Some((noted("ai_provider")?, noted("ai_model")?))
}

/// The mode `room` was left in, when it keeps one.
pub(crate) async fn kept_mode(kit: &RoomKit<SqliteStore>, room: &RoomId) -> Option<Mode> {
    kept(kit, room).await.mode.as_deref().and_then(Mode::named)
}

/// Keeps the mode of every conversation that has one of its own.
pub(crate) async fn keep_modes(kit: &RoomKit<SqliteStore>, policy: &Policy) {
    for (room, mode) in policy.chosen() {
        let Ok(room) = RoomId::new(room) else {
            continue;
        };
        // A mode that cannot be kept is the launch's at the next one.
        let _ = keep(kit, &room, |kept| kept.mode = Some(mode.value().into())).await;
    }
}

/// Keeps each conversation's mode as it changes, whoever changes it: the
/// person with `/mode`, the agent with its plan mode tools.
pub(crate) fn follow_modes(kit: &RoomKit<SqliteStore>, policy: &Policy) {
    let (kit, policy) = (kit.downgrade(), policy.clone());
    let mut changes = policy.changes.subscribe();
    tokio::spawn(async move {
        while changes.changed().await.is_ok() {
            let Some(kit) = kit.upgrade() else {
                break;
            };
            keep_modes(&kit, &policy).await;
        }
    });
}
