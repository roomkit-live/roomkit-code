//! The tasks of a conversation that earlier launches ran, as their rooms keep
//! them: its total and `/tasks` say the same after `/resume`.

use std::collections::HashMap;
use std::time::Instant;

use roomkit::RoomKit;
use roomkit::model::Room;
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
    let (mut children, whole) = task_rooms(kit, current.organization.clone()).await;
    if !whole {
        tasks.unread(current.room.as_str());
        let room = json!({"room": current.room.as_str()});
        profiles
            .diagnostics
            .record("tasks_not_restored", room, None);
    }
    let mut found = Vec::new();
    let mut rooms = vec![(current.room.as_str().to_owned(), 0)];
    while let Some((parent, depth)) = rooms.pop() {
        // The depth bound only guards against a cycle.
        if depth > MAX_DEPTH {
            continue;
        }
        for child in children.remove(&parent).unwrap_or_default() {
            found.extend(task(&child, roster));
            rooms.push((child.id.as_str().to_owned(), depth + 1));
        }
    }
    tasks.restored(found);
}

/// Every room that holds a task, by the room that handed it over, the oldest
/// first, and whether all of them could be read: what could be read when the
/// store fails. A store reads every room to filter them by metadata, so they
/// are read once, whatever the tasks.
async fn task_rooms(
    kit: &RoomKit<SqliteStore>,
    organization_id: Option<String>,
) -> (HashMap<String, Vec<Room>>, bool) {
    let (rooms, whole) = pages(PAGE, |offset| {
        let options = FindRoomsOptions {
            organization_id: organization_id.clone(),
            limit: PAGE,
            offset,
            ..FindRoomsOptions::default()
        };
        async move { kit.store().find_rooms(options).await }
    })
    .await;
    let mut by_parent: HashMap<String, Vec<Room>> = HashMap::new();
    for room in rooms {
        let parent = room.metadata.get("parent_room_id").and_then(Value::as_str);
        if let Some(parent) = parent.map(str::to_owned) {
            by_parent.entry(parent).or_default().push(room);
        }
    }
    for rooms in by_parent.values_mut() {
        rooms.sort_by_key(|room| room.created_at);
    }
    (by_parent, whole)
}

/// Every page `read` gives from an offset, `size` items each, until a shorter
/// one; and whether the last was reached, not a failure.
async fn pages<T, E, Page>(size: u64, mut read: impl FnMut(u64) -> Page) -> (Vec<T>, bool)
where
    Page: Future<Output = Result<Vec<T>, E>>,
{
    let mut items = Vec::new();
    loop {
        let Ok(page) = read(items.len() as u64).await else {
            return (items, false);
        };
        let last = (page.len() as u64) < size;
        items.extend(page);
        if last {
            return (items, true);
        }
    }
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
        input: text("task_input").unwrap_or_default().into(),
        started: Instant::now(),
        ended: Some((took, Outcome::recorded(status))),
        spent: None,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
    use super::*;

    #[tokio::test]
    async fn a_page_that_fails_says_the_rooms_are_not_all_read() {
        let (read, whole) = pages(2, |offset| async move {
            match offset {
                0 => Ok(vec![1, 2]),
                _ => Err("the store failed"),
            }
        })
        .await;
        assert_eq!(read, [1, 2]);
        assert!(!whole);
    }

    #[tokio::test]
    async fn a_short_page_ends_the_reading() {
        let (read, whole) = pages(2, |offset| async move {
            Ok::<_, &str>(match offset {
                0 => vec![1, 2],
                2 => vec![3],
                _ => vec![4, 5, 6],
            })
        })
        .await;
        assert_eq!(read, [1, 2, 3]);
        assert!(whole);
    }
}
