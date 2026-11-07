//! `/tasks`: the tasks a conversation's agents handed over, the task each one
//! was given and how it stands, its conversation as text, refreshed each
//! second while it runs, and a control to stop it (HITL a0875053). The
//! sidebar lists the same tasks, and the background status counts those that
//! run beside the processes. A view sees only its conversation's tasks, and
//! those their workers handed on. What they show is `render`'s.

use std::sync::Mutex;
use std::time::Duration;

use roomkit::{CancelTaskOptions, RoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{Event, SidebarSection};
use roomkit_tui_engine::{CommandResult, SessionConfig};

use super::{Outcome, Task, Tasks, render};

/// The command that opens the panel.
pub(crate) const COMMAND: &str = "tasks";

/// How often the panel is refreshed while it follows a task that runs.
const FOLLOW: Duration = Duration::from_secs(1);

/// How often the sidebar counts a running task's seconds.
const COUNT: Duration = Duration::from_secs(2);

/// The panel a view opened: one at a time, as the processes'.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Open {
    /// Fresh at each action, so a late refresh or a double press cannot act
    /// on what a previous one showed.
    pub(super) id: String,
    pub(super) room: String,
    pub(super) agent: String,
    /// The task shown, else the list.
    pub(super) selected: Option<String>,
}

/// The open panel, if any.
#[derive(Default)]
pub(crate) struct Panel(Mutex<Option<Open>>);

impl Panel {
    fn get(&self) -> Option<Open> {
        crate::sync::lock(&self.0).clone()
    }

    fn set(&self, open: Option<Open>) {
        *crate::sync::lock(&self.0) = open;
    }

    /// Forgets the open panel: a view attached shows none, and the terminal
    /// drops one without saying so.
    pub(crate) fn close(&self) {
        self.set(None);
    }

    #[cfg(test)]
    pub(crate) fn is_open(&self) -> bool {
        self.get().is_some()
    }
}

fn shown_in(open: &Open, current: &SessionConfig) -> bool {
    open.room == current.room.as_str() && open.agent == current.agent.as_str()
}

/// `/tasks [task-id]`, and the panel's own actions `<panel>:<action>`.
pub(crate) async fn handle(
    kit: &RoomKit<SqliteStore>,
    tasks: &Tasks,
    current: &SessionConfig,
    arguments: &str,
) -> Result<CommandResult, String> {
    let room = current.room.as_str();
    let mut open = match arguments.split_once(':') {
        None => {
            let selected = (!arguments.is_empty()).then(|| arguments.to_owned());
            if let Some(id) = &selected
                && tasks.find(room, id).is_none()
            {
                return Err("This conversation has no such task".into());
            }
            Open {
                id: String::new(),
                room: room.to_owned(),
                agent: current.agent.to_string(),
                selected,
            }
        }
        Some((id, action)) => {
            let open = tasks
                .panel
                .get()
                .filter(|open| open.id == id && shown_in(open, current))
                .ok_or("This task action belongs to an older view")?;
            match act(kit, tasks, current, open, action).await? {
                Some(open) => open,
                None => {
                    tasks.panel.close();
                    return Ok(CommandResult::Events(Vec::new()));
                }
            }
        }
    };
    open.id = uuid::Uuid::new_v4().to_string();
    let spec = render::panel(kit, tasks, &open, current.organization.clone()).await;
    tasks.panel.set(Some(open));
    Ok(CommandResult::Events(vec![Event::Panel {
        conversation: room.into(),
        panel: spec,
    }]))
}

/// What `action` leaves the panel showing, or none once it is dismissed.
async fn act(
    kit: &RoomKit<SqliteStore>,
    tasks: &Tasks,
    current: &SessionConfig,
    mut open: Open,
    action: &str,
) -> Result<Option<Open>, String> {
    let room = current.room.as_str();
    match action {
        "dismiss" => return Ok(None),
        "list" => open.selected = None,
        "stop" => {
            let task = open
                .selected
                .as_deref()
                .and_then(|id| tasks.find(room, id))
                .ok_or("This conversation has no such task")?;
            stop(kit, tasks, current, task).await?;
        }
        _ => {
            let id = action.strip_prefix("open:").ok_or("Unknown task action")?;
            if tasks.find(room, id).is_none() {
                return Err("This conversation has no such task".into());
            }
            open.selected = Some(id.to_owned());
        }
    }
    Ok(Some(open))
}

/// Cancels `task`, which the engine ends cancelled: its worker stops, and
/// the agent that handed it over is told.
async fn stop(
    kit: &RoomKit<SqliteStore>,
    tasks: &Tasks,
    current: &SessionConfig,
    task: Task,
) -> Result<(), String> {
    let options = CancelTaskOptions {
        organization_id: current.organization.clone(),
    };
    if !task.running() || !kit.cancel_task(&task.id, options).await {
        return Err("This task already ended".into());
    }
    // The engine's hook records the end too, but runs apart: the panel shows
    // the task stopped at once.
    let took = task.started.elapsed();
    tasks.ended(Task {
        ended: Some((took, Outcome::Cancelled)),
        ..task
    });
    Ok(())
}

/// What a view's live events show of its conversation's tasks.
pub(crate) struct Live {
    /// The open panel as it stands now.
    pub(crate) panel: Option<Event>,
    pub(crate) section: SidebarSection,
    /// How soon the view needs them again, if it does.
    pub(crate) poll: Option<Duration>,
}

pub(crate) async fn live(
    kit: &RoomKit<SqliteStore>,
    tasks: &Tasks,
    current: &SessionConfig,
) -> Live {
    let room = current.room.as_str();
    let update = update(kit, tasks, current).await;
    let follows = update.as_ref().is_some_and(|(_, follows)| *follows);
    let counting = tasks.of(room).iter().any(|(_, task)| task.running());
    Live {
        panel: update.map(|(event, _)| event),
        section: render::sidebar(tasks, room),
        poll: poll(follows, counting),
    }
}

/// Each second while the panel follows a task that runs, every two while the
/// sidebar counts one's seconds, else never.
fn poll(follows: bool, counting: bool) -> Option<Duration> {
    if follows {
        return Some(FOLLOW);
    }
    counting.then_some(COUNT)
}

/// The open panel as it stands now, for `current`'s view if it has it open,
/// and whether it follows a task that runs.
async fn update(
    kit: &RoomKit<SqliteStore>,
    tasks: &Tasks,
    current: &SessionConfig,
) -> Option<(Event, bool)> {
    let open = tasks.panel.get().filter(|open| shown_in(open, current))?;
    let follows = open
        .selected
        .as_deref()
        .and_then(|id| tasks.find(&open.room, id))
        .is_some_and(|task| task.running());
    let organization = current.organization.clone();
    let event = Event::PanelUpdated {
        conversation: current.room.as_str().into(),
        panel: render::panel(kit, tasks, &open, organization).await,
    };
    Some((event, follows))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panel follows a running task each second; the sidebar counts one's
    /// seconds every two; nothing polls without a task that runs.
    #[test]
    fn the_view_is_polled_only_while_a_task_runs() {
        assert_eq!(poll(true, true), Some(FOLLOW));
        assert_eq!(poll(false, true), Some(COUNT));
        assert_eq!(poll(false, false), None);
    }
}
