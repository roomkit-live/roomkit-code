//! `/tasks`: the tasks a conversation's agents handed over, the task each one
//! was given and how it stands, its conversation as text, refreshed each
//! second while it runs, and a control to stop it (HITL a0875053). The
//! sidebar lists the same tasks, and the background status counts those that
//! run beside the processes. A view sees only its conversation's tasks, and
//! those their workers handed on.

use std::sync::Mutex;
use std::time::Duration;

use roomkit::{CancelTaskOptions, RoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{
    Choice, Event, PanelSpec, SidebarContent, SidebarEntry, SidebarSection, SidebarTone,
};
use roomkit_tui_engine::{CommandResult, SessionConfig};

use super::{Outcome, Task, Tasks};

/// The command that opens the panel.
pub(crate) const COMMAND: &str = "tasks";

/// How many ended tasks the sidebar lists under those that run.
const ENDED_LISTED: usize = 3;

/// The panel a view opened: one at a time, as the processes'.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Open {
    /// Fresh at each action, so a late refresh or a double press cannot act
    /// on what a previous one showed.
    id: String,
    room: String,
    agent: String,
    /// The task shown, else the list.
    selected: Option<String>,
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
}

fn shown_in(open: &Open, current: &SessionConfig) -> bool {
    open.room == current.room.as_str() && open.agent == current.agent.as_str()
}

/// `/tasks [task-id]`, and the panel's own actions `<panel>:<action>`.
pub(crate) async fn handle(
    kit: &RoomKit<SqliteStore>,
    tasks: &Tasks,
    panel: &Panel,
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
            let open = panel
                .get()
                .filter(|open| open.id == id && shown_in(open, current))
                .ok_or("This task action belongs to an older view")?;
            match act(kit, tasks, current, open, action).await? {
                Some(open) => open,
                None => {
                    panel.set(None);
                    return Ok(CommandResult::Events(Vec::new()));
                }
            }
        }
    };
    open.id = uuid::Uuid::new_v4().to_string();
    let spec = render(kit, tasks, &open, current.organization.clone()).await;
    panel.set(Some(open));
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
            if !task.running() {
                return Err("This task already ended".into());
            }
            let options = CancelTaskOptions {
                organization_id: current.organization.clone(),
            };
            if !kit.cancel_task(&task.id, options).await {
                return Err("This task already ended".into());
            }
            // The engine's hook records the end too, but runs apart: the
            // panel shows the task stopped at once.
            let took = task.started.elapsed();
            tasks.ended(Task {
                ended: Some((took, Outcome::Cancelled)),
                ..task
            });
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

/// What a view's live events show of its conversation's tasks.
pub(crate) struct Live {
    /// The open panel as it stands now.
    pub(crate) panel: Option<Event>,
    pub(crate) section: SidebarSection,
    /// How soon the view needs them again: each second while the panel
    /// follows a task that runs, every two while the sidebar counts one's
    /// seconds.
    pub(crate) poll: Option<Duration>,
}

pub(crate) async fn live(
    kit: &RoomKit<SqliteStore>,
    tasks: &Tasks,
    current: &SessionConfig,
) -> Live {
    let room = current.room.as_str();
    let update = update(kit, tasks, &tasks.panel, current).await;
    let follows = update.as_ref().is_some_and(|(_, follows)| *follows);
    let counting = tasks.of(room).iter().any(|(_, task)| task.running());
    let poll = if follows {
        Some(Duration::from_secs(1))
    } else {
        counting.then(|| Duration::from_secs(2))
    };
    Live {
        panel: update.map(|(event, _)| event),
        section: sidebar(tasks, room),
        poll,
    }
}

/// The open panel as it stands now, for `current`'s view if it has it open,
/// and whether it follows a task that runs.
async fn update(
    kit: &RoomKit<SqliteStore>,
    tasks: &Tasks,
    panel: &Panel,
    current: &SessionConfig,
) -> Option<(Event, bool)> {
    let open = panel.get().filter(|open| shown_in(open, current))?;
    let follows = open
        .selected
        .as_deref()
        .and_then(|id| tasks.find(&open.room, id))
        .is_some_and(|task| task.running());
    let event = Event::PanelUpdated {
        conversation: current.room.as_str().into(),
        panel: render(kit, tasks, &open, current.organization.clone()).await,
    };
    Some((event, follows))
}

async fn render(
    kit: &RoomKit<SqliteStore>,
    tasks: &Tasks,
    open: &Open,
    organization: Option<String>,
) -> PanelSpec {
    let mut spec = PanelSpec::new(open.id.clone(), COMMAND, "Tasks · this conversation", "")
        .with_dismiss(format!("{}:dismiss", open.id));
    let action = |value: &str, label: &str, description: &str| {
        Choice::new(format!("{}:{value}", open.id), label).with_description(description)
    };
    let Some(id) = &open.selected else {
        spec.body = "Tasks this conversation's agents handed to other agents, and those they \
            handed on, since the application started. Closing the application stops the \
            tasks that run.\n\n"
            .into();
        let listed = tasks.of(&open.room);
        if listed.is_empty() {
            spec.body.push_str("No tasks.\n");
        }
        for (depth, task) in listed {
            let indent = "  ".repeat(depth);
            let label = format!("{indent}{} · {}", task.agent, task.state());
            spec.body
                .push_str(&format!("{label}\n{indent}  {}\n", task.preview()));
            spec.actions.push(action(
                &format!("open:{}", task.id),
                &label,
                &task.preview(),
            ));
        }
        return spec;
    };
    spec.actions.push(action("list", "Back to task list", ""));
    let Some(task) = tasks.find(&open.room, id) else {
        spec.body = "This task is no longer listed.".into();
        return spec;
    };
    spec.title = format!("Task for {} · {}", task.agent, task.state());
    spec.body = details(&task);
    spec.body.push_str("\nIts conversation, the latest last:\n");
    let conversation = super::transcript::read(kit, &task.room, &task.agent, organization);
    spec.body.push_str(&conversation.await);
    if task.running() {
        spec.actions.push(action(
            "stop",
            "Stop task",
            "Cancel this task; its agent stops working on it",
        ));
    }
    spec
}

fn details(task: &Task) -> String {
    format!(
        "Agent: {}\nState: {}\nConversation: {}\n\nTask:\n{}\n",
        task.agent,
        task.state(),
        task.room,
        task.input
    )
}

/// The sidebar's list of `room`'s tasks: those that run, then the latest
/// ended.
fn sidebar(tasks: &Tasks, room: &str) -> SidebarSection {
    let listed: Vec<Task> = tasks.of(room).into_iter().map(|(_, task)| task).collect();
    let running = listed.iter().filter(|task| task.running());
    let ended = listed.iter().rev().filter(|task| !task.running());
    let mut entries: Vec<SidebarEntry> = running
        .chain(ended.take(ENDED_LISTED))
        .map(|task| {
            let tone = if task.running() {
                SidebarTone::Accent
            } else {
                SidebarTone::Muted
            };
            let mark = if task.running() { "●" } else { "○" };
            SidebarEntry::new(format!("{mark} {}", clip(&task.agent, 60)))
                .with_detail(clip(&task.state(), 64))
                .with_tone(tone)
                .with_action(crate::profiles::sidebar::action(COMMAND, task.id.clone()))
        })
        .collect();
    if entries.is_empty() {
        entries.push(SidebarEntry::new("No tasks").with_tone(SidebarTone::Muted));
    }
    SidebarSection::new("Tasks", SidebarContent::Entries { entries })
        .with_action(crate::profiles::sidebar::action(COMMAND, ""))
}

fn clip(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}
