//! The tasks of a conversation that earlier launches ran, as their rooms keep
//! them: its total and `/tasks` say the same after `/resume`.

use std::time::Instant;

use roomkit::RoomKit;
use roomkit::model::{Room, RoomId};
use roomkit::store::{ConversationStore, FindRoomsOptions};
use roomkit_sqlite::SqliteStore;
use roomkit_tui_engine::SessionConfig;
use serde_json::{Value, json};

use super::{MAX_DEPTH, Outcome, Roster, Task};
use crate::profiles::Profiles;

/// Rooms read at a time.
const PAGE: u64 = 200;

/// Lists the tasks `current`'s conversation ran in earlier launches, and
/// those their workers handed on, the first time a view shows it.
pub(crate) async fn restore(
    kit: &RoomKit<SqliteStore>,
    profiles: &Profiles,
    current: &SessionConfig,
) {
    let (tasks, roster) = (&profiles.resources.tasks, &profiles.resources.roster);
    if !tasks.restoring(current.room.as_str()) {
        return;
    }
    let mut found = Vec::new();
    let mut rooms = vec![(current.room.clone(), 0)];
    while let Some((parent, depth)) = rooms.pop() {
        // The depth bound only guards against a cycle.
        if depth > MAX_DEPTH {
            continue;
        }
        for child in children(kit, &parent, current.organization.clone()).await {
            found.extend(task(&child, roster));
            rooms.push((child.id, depth + 1));
        }
    }
    tasks.restored(found);
}

/// The rooms of the tasks handed over in `parent`, the oldest first; what
/// could be read when the store fails.
async fn children(
    kit: &RoomKit<SqliteStore>,
    parent: &RoomId,
    organization_id: Option<String>,
) -> Vec<Room> {
    let mut rooms = Vec::new();
    let metadata_filter = roomkit::model::Metadata::from_iter([(
        "parent_room_id".to_owned(),
        json!(parent.as_str()),
    )]);
    loop {
        let options = FindRoomsOptions {
            organization_id: organization_id.clone(),
            metadata_filter: Some(metadata_filter.clone()),
            limit: PAGE,
            offset: rooms.len() as u64,
            ..FindRoomsOptions::default()
        };
        let Ok(page) = kit.store().find_rooms(options).await else {
            break;
        };
        let last = (page.len() as u64) < PAGE;
        rooms.extend(page);
        if last {
            break;
        }
    }
    rooms.sort_by_key(|room| room.created_at);
    rooms
}

/// The task `room` keeps; none when the room's metadata does not name one.
fn task(room: &Room, roster: &Roster) -> Option<Task> {
    let text = |key: &str| room.metadata.get(key).and_then(Value::as_str);
    let agent_id = text("task_agent_id")?;
    let took = room
        .updated_at
        .duration_since(room.created_at)
        .unwrap_or_default();
    let status = text("task_status").unwrap_or_default();
    Some(Task {
        id: text("task_id")?.to_owned(),
        parent: text("parent_room_id")?.to_owned(),
        room: room.id.as_str().to_owned(),
        agent: roster.label(agent_id),
        agent_id: agent_id.to_owned(),
        input: text("task_input").unwrap_or_default().to_owned(),
        started: Instant::now(),
        ended: Some((took, Outcome::recorded(status))),
        spent: None,
    })
}
