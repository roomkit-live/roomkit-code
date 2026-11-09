//! Moving a view between a conversation and the rooms of its tasks (RMK-1535;
//! HITL 6e1f8f04): `/tasks` opens one, Alt+Right and Alt+Left go around them.
//! There the person follows the worker and writes to it. What they say to a
//! worker whose task ended stays in its room.

use roomkit::model::{Access, ChannelId, RoomId};
use roomkit::{AttachChannelOptions, ListBindingsOptions};
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
    bind_terminal(sessions, &room, current.organization.clone()).await?;
    let mut next = current.clone();
    next.room = room.clone();
    next.agent = agent;
    next.title = format!("Task for {} · Alt+Left to go back", task.agent);
    next.mode = Some(sessions.policy.state(room.as_str()));
    sessions.profiles.configure_view(&mut next).await;
    Ok(CommandResult::Switch(Box::new(next)))
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
    let tasks = &sessions.profiles.resources.tasks;
    let mut next = current.clone();
    if let Some(agent) = tasks
        .lead_of(root)
        .and_then(|agent| ChannelId::new(agent).ok())
    {
        next.agent = agent;
    }
    sessions.profiles.configure_view(&mut next).await;
    Ok(sessions.switched(next, room).await)
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
