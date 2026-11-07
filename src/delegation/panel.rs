//! `/tasks`: the tasks a conversation's agents handed over, the task each one
//! was given and how it stands, its conversation as text, refreshed each
//! second while it runs, and a control to stop it (HITL a0875053). The
//! sidebar lists the same tasks, and the background status counts those that
//! run beside the processes. A view sees only its conversation's tasks, and
//! those their workers handed on.

use std::collections::HashSet;
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

/// How many tasks the list offers to open: the terminal shows no panel of
/// more than 64 actions.
const LISTED: usize = 60;

/// How many characters of an agent's label a panel or the sidebar shows: a
/// label the terminal takes holds 256 bytes at most.
const AGENT_CHARS: usize = 40;

/// How many bytes of a task's text the panel shows above its conversation.
const INPUT_BYTES: usize = 4096;

/// How often the panel is refreshed while it follows a task that runs.
const FOLLOW: Duration = Duration::from_secs(1);

/// How often the sidebar counts a running task's seconds.
const COUNT: Duration = Duration::from_secs(2);

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
    let spec = render(kit, tasks, &open, current.organization.clone()).await;
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
        section: sidebar(tasks, room),
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
    let spec = PanelSpec::new(open.id.clone(), COMMAND, "Tasks · this conversation", "")
        .with_dismiss(format!("{}:dismiss", open.id));
    match &open.selected {
        None => list(spec, tasks, open),
        Some(id) => detail(spec, kit, tasks, open, id, organization).await,
    }
}

fn action(open: &Open, value: &str, label: &str, description: &str) -> Choice {
    Choice::new(format!("{}:{value}", open.id), label).with_description(description)
}

/// The conversation's tasks, at most [`LISTED`]: those that run, then the
/// latest ended, each after the task that handed it on.
fn list(mut spec: PanelSpec, tasks: &Tasks, open: &Open) -> PanelSpec {
    spec.body = "Tasks this conversation's agents handed to other agents, and those they \
        handed on, the latest ended among them. Closing the application stops the tasks \
        that run.\n\n"
        .into();
    let listed = tasks.of(&open.room);
    if listed.is_empty() {
        spec.body.push_str("No tasks.\n");
    }
    let shown = shown(&listed);
    for (depth, task) in listed.iter().filter(|(_, task)| shown.contains(&task.id)) {
        let indent = "  ".repeat(*depth);
        let label = format!(
            "{indent}{} · {}",
            clip(&task.agent, AGENT_CHARS),
            task.state()
        );
        spec.body
            .push_str(&format!("{label}\n{indent}  {}\n", task.preview()));
        let value = format!("open:{}", task.id);
        spec.actions
            .push(action(open, &value, &label, &task.preview()));
    }
    let hidden = listed.len() - shown.len();
    if hidden > 0 {
        spec.body
            .push_str(&format!("\n{hidden} older ended tasks are not listed.\n"));
    }
    spec
}

/// The IDs of the tasks the list shows: every one that runs, then the latest
/// ended, [`LISTED`] in all.
fn shown(listed: &[(usize, Task)]) -> HashSet<String> {
    let running = listed.iter().filter(|(_, task)| task.running());
    let ended = listed.iter().rev().filter(|(_, task)| !task.running());
    running
        .chain(ended)
        .take(LISTED)
        .map(|(_, task)| task.id.clone())
        .collect()
}

/// Task `id`: its agent, state and task, then its conversation as text.
async fn detail(
    mut spec: PanelSpec,
    kit: &RoomKit<SqliteStore>,
    tasks: &Tasks,
    open: &Open,
    id: &str,
    organization: Option<String>,
) -> PanelSpec {
    spec.actions
        .push(action(open, "list", "Back to task list", ""));
    let Some(task) = tasks.find(&open.room, id) else {
        spec.body = "This task is no longer listed.".into();
        return spec;
    };
    let agent = clip(&task.agent, AGENT_CHARS);
    spec.title = format!("Task for {agent} · {}", task.state());
    spec.body = details(&task);
    spec.body.push_str("\nIts conversation, the latest last:\n");
    let conversation = super::transcript::read(kit, &task.room, &agent, organization);
    spec.body.push_str(&conversation.await);
    if task.running() {
        let description = "Cancel this task; its agent stops working on it";
        spec.actions
            .push(action(open, "stop", "Stop task", description));
    }
    spec
}

fn details(task: &Task) -> String {
    let mut input = cut(&task.input, INPUT_BYTES).to_owned();
    if input.len() < task.input.len() {
        input.push_str("… (cut)");
    }
    format!(
        "Agent: {}\nState: {}\nConversation: {}\n\nTask:\n{input}\n",
        clip(&task.agent, AGENT_CHARS),
        task.state(),
        task.room,
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
            SidebarEntry::new(format!("{mark} {}", clip(&task.agent, AGENT_CHARS)))
                .with_detail(task.state())
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

/// `text` within `bytes`, cut at a character's boundary.
fn cut(text: &str, bytes: usize) -> &str {
    if text.len() <= bytes {
        return text;
    }
    let end = (0..=bytes)
        .rev()
        .find(|index| text.is_char_boundary(*index))
        .unwrap_or_default();
    &text[..end]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, reason = "A broken fixture fails the test")]
    use std::time::Instant;

    use super::*;

    fn task(id: &str, ended: bool) -> Task {
        Task {
            id: id.into(),
            parent: "main".into(),
            room: format!("main::{id}"),
            agent: "Review".into(),
            input: format!("Task {id}"),
            started: Instant::now(),
            ended: ended.then_some((Duration::from_secs(3), Outcome::Completed)),
        }
    }

    fn open(selected: Option<&str>) -> Open {
        Open {
            id: "p1".into(),
            room: "main".into(),
            agent: "code".into(),
            selected: selected.map(String::from),
        }
    }

    /// The panel follows a running task each second; the sidebar counts one's
    /// seconds every two; nothing polls without a task that runs.
    #[test]
    fn the_view_is_polled_only_while_a_task_runs() {
        assert_eq!(poll(true, true), Some(FOLLOW));
        assert_eq!(poll(false, true), Some(COUNT));
        assert_eq!(poll(false, false), None);
    }

    /// The sidebar lists every task that runs, then the three latest ended.
    #[test]
    fn the_sidebar_lists_running_tasks_then_the_latest_ended() {
        let tasks = Tasks::default();
        for n in 0..5 {
            tasks.ended(task(&format!("e{n}"), true));
        }
        tasks.started(task("r", false));
        let SidebarContent::Entries { entries } = sidebar(&tasks, "main").content else {
            panic!("Expected entries");
        };
        let details: Vec<&str> = entries.iter().map(|e| e.detail.as_str()).collect();
        assert_eq!(entries.len(), 1 + ENDED_LISTED, "{details:?}");
        assert!(details[0].starts_with("running"), "{details:?}");
        let SidebarContent::Entries { entries } = sidebar(&tasks, "other").content else {
            panic!("Expected entries");
        };
        assert_eq!(entries[0].label, "No tasks");
    }

    /// The list offers at most [`LISTED`] tasks, every one that runs among
    /// them, and says how many it leaves out.
    #[test]
    fn the_list_stays_within_the_terminal_s_actions() {
        let tasks = Tasks::default();
        for n in 0..15 {
            tasks.started(task(&format!("r{n}"), false));
        }
        for n in 0..50 {
            tasks.ended(task(&format!("e{n}"), true));
        }
        let spec = list(PanelSpec::new("p1", COMMAND, "", ""), &tasks, &open(None));
        assert_eq!(spec.actions.len(), LISTED);
        let running = spec.actions.iter().filter(|a| a.value.contains(":open:r"));
        assert_eq!(running.count(), 15);
        assert!(
            spec.body
                .contains("\n5 older ended tasks are not listed.\n")
        );
    }

    /// A long task and a long label are cut to what the terminal takes.
    #[test]
    fn long_texts_are_cut_to_the_terminal_s_limits() {
        let mut long = task("t", false);
        long.input = "é".repeat(INPUT_BYTES);
        long.agent = "a".repeat(300);
        let text = details(&long);
        assert!(text.len() < INPUT_BYTES + 400, "{}", text.len());
        assert!(text.contains("… (cut)"));
        let tasks = Tasks::default();
        tasks.started(long);
        let spec = list(PanelSpec::new("p1", COMMAND, "", ""), &tasks, &open(None));
        assert!(spec.actions[0].label.len() <= 256);
    }
}
