//! Moving a view between a conversation and the rooms of its tasks (RMK-1535;
//! HITL 6e1f8f04): `/tasks` opens one, Alt+Right and Alt+Left go around them.
//! There the person follows the worker and writes to it. What they say to a
//! worker whose task ended stays in its room.

use roomkit::model::{Access, ChannelId, RoomId};
use roomkit::{AttachChannelOptions, GetRoomOptions, ListBindingsOptions};
use roomkit_tui_engine::{CommandResult, SessionConfig};

use super::Sessions;
use crate::delegation::Task;

/// A view of `task`'s room, whose agent it shows. A task of an earlier version
/// has no terminal in its room, which opening it brings.
pub(crate) async fn open_task(
    sessions: &Sessions,
    current: &SessionConfig,
    task: &Task,
) -> Result<CommandResult, String> {
    let room = RoomId::new(&task.room).map_err(|_| "This task's room cannot be opened")?;
    let agent = ChannelId::new(&task.agent_id).map_err(|_| "This task's agent is unknown")?;
    let organization = current.organization.clone();
    bind_terminal(sessions, &room, organization.clone()).await?;
    adopt(sessions, &room, organization).await?;
    let root = sessions.profiles.resources.tasks.root_of(room.as_str());
    let mut next = current.clone();
    next.room = room.clone();
    next.agent = agent;
    next.title = format!("Task for {} · /back returns", task.agent);
    next.mode = Some(sessions.policy.state(room.as_str()));
    next.group = Some(root);
    // Stopping the run stops the task: a message never cuts it to be sent.
    next.can_cut_to_send = !task.running();
    ready(sessions, &mut next).await;
    sessions.profiles.resources.tasks.tell(
        room.as_str(),
        format!(
            "You are in {}'s room: /back returns to the conversation",
            task.agent
        ),
    );
    Ok(CommandResult::Switch(Box::new(next)))
}

/// Configures `view`, assembling its agent when it is not ready: a worker of
/// an earlier launch was never served, and only a task's start assembles one.
async fn ready(sessions: &Sessions, view: &mut SessionConfig) {
    sessions.profiles.configure_view(view).await;
    if view.submission_error.is_none() {
        return;
    }
    // One that cannot be assembled leaves the room readable: the view says
    // why it cannot be written to.
    let agent = view.agent.to_string();
    let prepared = sessions.profiles.prepare_target(&sessions.kit, &agent);
    if prepared.await.is_ok() {
        sessions.profiles.configure_view(view).await;
    }
}

/// Tells the policy `room` and the rooms it was handed on from are tasks',
/// so the mode shown from it is its conversation's before an agent's next
/// turn says so. The depth bound only guards against a cycle.
async fn adopt(
    sessions: &Sessions,
    room: &RoomId,
    organization_id: Option<String>,
) -> Result<(), String> {
    let mut current = room.clone();
    for _ in 0..=crate::delegation::MAX_DEPTH {
        let options = GetRoomOptions {
            organization_id: organization_id.clone(),
        };
        let found = sessions
            .kit
            .get_room(&current, options)
            .await
            .map_err(|_| "This task's room cannot be read")?;
        sessions.policy.adopt(&found);
        let parent = found
            .metadata
            .get("parent_room_id")
            .and_then(|p| p.as_str());
        let Some(parent) = parent.and_then(|parent| RoomId::new(parent).ok()) else {
            break;
        };
        current = parent;
    }
    Ok(())
}

/// `current` as a view of its conversation: what a command that changes the
/// conversation acts from when the view is in a task's room.
pub(crate) async fn in_conversation(
    sessions: &Sessions,
    current: SessionConfig,
) -> Result<SessionConfig, String> {
    let tasks = &sessions.profiles.resources.tasks;
    let root = tasks.root_of(current.room.as_str());
    if root == current.room.as_str() {
        return Ok(current);
    }
    let room = RoomId::new(&root).map_err(|_| "This conversation cannot be opened")?;
    let mut next = current;
    next.room = room;
    home(sessions, &mut next, &root).await;
    Ok(next)
}

/// Sets `view`'s agent and settings to those of conversation `root`.
async fn home(sessions: &Sessions, view: &mut SessionConfig, root: &str) {
    let tasks = &sessions.profiles.resources.tasks;
    if let Some(agent) = tasks
        .lead_of(root)
        .and_then(|agent| ChannelId::new(agent).ok())
    {
        view.agent = agent;
    }
    view.group = None;
    view.can_cut_to_send = true;
    sessions.profiles.configure_view(view).await;
}

async fn bind_terminal(
    sessions: &Sessions,
    room: &RoomId,
    organization_id: Option<String>,
) -> Result<(), String> {
    let options = ListBindingsOptions { organization_id };
    let bindings = sessions
        .kit
        .list_bindings(room, options)
        .await
        .map_err(|_| "This task's room cannot be read")?;
    let bound = bindings.iter().any(|binding| {
        binding.channel_id == sessions.channel && binding.access == Access::ReadWrite
    });
    if bound {
        return Ok(());
    }
    sessions
        .kit
        .attach_channel(room, &sessions.channel, AttachChannelOptions::default())
        .await
        .map(|_| ())
        .map_err(|_| "This task's room cannot be opened".to_owned())
}

/// A view of the conversation `root`, with the agent it last showed.
async fn open_conversation(
    sessions: &Sessions,
    current: &SessionConfig,
    root: &str,
) -> Result<CommandResult, String> {
    let room = RoomId::new(root).map_err(|_| "This conversation cannot be opened")?;
    let mut next = current.clone();
    home(sessions, &mut next, root).await;
    Ok(sessions.switched(next, room).await)
}

/// `/back`: from a task's room, to its conversation.
pub(crate) async fn back(
    sessions: &Sessions,
    current: &SessionConfig,
) -> Result<CommandResult, String> {
    let root = sessions
        .profiles
        .resources
        .tasks
        .root_of(current.room.as_str());
    if root == current.room.as_str() {
        return Err("You are already in the conversation".into());
    }
    open_conversation(sessions, current, &root).await
}

/// Moves to room `target` of the conversation shown: its own, or a task's.
pub(crate) async fn open(
    sessions: &Sessions,
    current: &SessionConfig,
    target: &str,
) -> Result<CommandResult, String> {
    let tasks = &sessions.profiles.resources.tasks;
    let root = tasks.root_of(current.room.as_str());
    if target == root {
        return open_conversation(sessions, current, &root).await;
    }
    let listed = tasks.of(&root);
    let task = listed
        .iter()
        .map(|(_, task)| task)
        .find(|task| task.room == target);
    match task {
        Some(task) => open_task(sessions, current, task).await,
        None => Err("This conversation has no such room".into()),
    }
}

/// Moves to the next room around the conversation, or the previous one: the
/// conversation and the tasks that run, with the room shown wherever it is.
pub(crate) async fn cycle(
    sessions: &Sessions,
    current: &SessionConfig,
    forward: bool,
) -> Result<CommandResult, String> {
    let tasks = &sessions.profiles.resources.tasks;
    let room = current.room.as_str();
    let root = tasks.root_of(room);
    let ring: Vec<Task> = tasks
        .of(&root)
        .into_iter()
        .map(|(_, task)| task)
        .filter(|task| task.running() || task.room == room)
        .collect();
    if ring.is_empty() {
        return Err("No task is running in this conversation".into());
    }
    // 0 is the conversation; each task follows, by its place plus one.
    let at = ring
        .iter()
        .position(|task| task.room == room)
        .map_or(0, |at| at + 1);
    let stops = ring.len() + 1;
    let next = if forward { at + 1 } else { at + stops - 1 } % stops;
    match next.checked_sub(1).and_then(|task| ring.get(task)) {
        Some(task) => open_task(sessions, current, task).await,
        None => open_conversation(sessions, current, &root).await,
    }
}
