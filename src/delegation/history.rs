//! The tasks of a conversation that earlier launches ran, as their rooms keep
//! them: its total and `/tasks` say the same after `/resume`.

use std::collections::HashMap;
use std::time::Instant;

use roomkit::RoomKit;
use roomkit::model::Room;
use roomkit::store::{ConversationStore, FindRoomsOptions};
use roomkit_sqlite::SqliteStore;
use roomkit_tui_engine::SessionConfig;
use serde_json::Value;

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
    let mut children = task_rooms(kit, current.organization.clone()).await;
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
/// first; what could be read when the store fails. A store reads every room
/// to filter them by metadata, so they are read once, whatever the tasks.
async fn task_rooms(
    kit: &RoomKit<SqliteStore>,
    organization_id: Option<String>,
) -> HashMap<String, Vec<Room>> {
    let mut by_parent: HashMap<String, Vec<Room>> = HashMap::new();
    let mut offset = 0;
    loop {
        let options = FindRoomsOptions {
            organization_id: organization_id.clone(),
            limit: PAGE,
            offset,
            ..FindRoomsOptions::default()
        };
        let Ok(page) = kit.store().find_rooms(options).await else {
            break;
        };
        let last = (page.len() as u64) < PAGE;
        offset += page.len() as u64;
        for room in page {
            let parent = room.metadata.get("parent_room_id").and_then(Value::as_str);
            if let Some(parent) = parent.map(str::to_owned) {
                by_parent.entry(parent).or_default().push(room);
            }
        }
        if last {
            break;
        }
    }
    for rooms in by_parent.values_mut() {
        rooms.sort_by_key(|room| room.created_at);
    }
    by_parent
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
    use roomkit::model::RoomId;
    use serde_json::json;

    use super::*;

    fn room(status: &str) -> Room {
        let mut room = Room::new(RoomId::new("main::task-1").unwrap());
        room.created_at = "2026-11-09T10:00:00Z".parse().unwrap();
        room.updated_at = "2026-11-09T10:00:42Z".parse().unwrap();
        room.metadata = roomkit::model::Metadata::from_iter([
            ("task_id".to_owned(), json!("t1")),
            ("parent_room_id".to_owned(), json!("main")),
            ("task_agent_id".to_owned(), json!("review")),
            ("task_input".to_owned(), json!("Review the diff")),
            ("task_status".to_owned(), json!(status)),
        ]);
        room
    }

    /// A room keeps who worked, on what, for how long and how it ended.
    #[test]
    fn a_room_reads_back_as_its_task() {
        let task = task(&room("completed"), &Roster::default()).unwrap();
        assert_eq!(
            (&*task.id, &*task.parent, &*task.room, &*task.agent_id),
            ("t1", "main", "main::task-1", "review")
        );
        assert_eq!(&*task.input, "Review the diff");
        assert_eq!(task.state(), "completed in 42 s");
    }

    /// One a crash left running says so, with no duration it cannot know.
    #[test]
    fn a_task_a_crash_left_running_reads_interrupted() {
        let task = task(&room("in_progress"), &Roster::default()).unwrap();
        assert!(task.interrupted());
        assert_eq!(task.state(), "interrupted");
    }

    #[test]
    fn a_room_that_names_no_task_is_none() {
        let mut plain = room("completed");
        plain.metadata.remove("task_id");
        assert!(task(&plain, &Roster::default()).is_none());
    }
}
