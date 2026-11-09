//! What `/tasks` and the sidebar show of a conversation's tasks, within what
//! the terminal takes: a panel of 64 actions and 64 KiB of text at most, and
//! labels of 256 bytes.

use std::collections::HashSet;

use roomkit::RoomKit;
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{
    Choice, PanelSpec, SidebarContent, SidebarEntry, SidebarSection, SidebarTone,
};

use super::panel::{COMMAND, Open};
use super::{Task, Tasks};

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

/// The panel `open` shows: the list of tasks, or the task it selected.
pub(super) async fn panel(
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
        let agent = clip(&task.agent, AGENT_CHARS);
        let label = format!("{indent}{agent} · {}", task.summary());
        spec.body
            .push_str(&format!("{label}\n{indent}  {}\n", task.preview()));
        let value = format!("open:{}", task.id);
        spec.actions
            .push(action(open, &value, &label, &task.preview()));
    }
    match listed.len() - shown.len() {
        0 => {}
        1 => spec.body.push_str("\n1 more task is not listed.\n"),
        hidden => spec
            .body
            .push_str(&format!("\n{hidden} more tasks are not listed.\n")),
    }
    spec
}

/// The IDs of the tasks the list shows, [`LISTED`] at most: every one that
/// runs, then the latest ended, each with the tasks it was handed on from,
/// which place it.
fn shown(listed: &[(usize, Task)]) -> HashSet<String> {
    let running = (0..listed.len()).filter(|&index| listed[index].1.running());
    let ended = (0..listed.len())
        .rev()
        .filter(|&index| !listed[index].1.running());
    let mut shown = HashSet::new();
    for index in running.chain(ended) {
        let new: Vec<String> = lineage(listed, index)
            .into_iter()
            .map(|line| listed[line].1.id.clone())
            .filter(|id| !shown.contains(id))
            .collect();
        if shown.len() + new.len() > LISTED {
            break;
        }
        shown.extend(new);
    }
    shown
}

/// Entry `index` of `listed`, which is in tree order, and the entries it was
/// handed on from.
fn lineage(listed: &[(usize, Task)], index: usize) -> Vec<usize> {
    let mut line = vec![index];
    let mut depth = listed[index].0;
    for earlier in (0..index).rev() {
        if depth == 0 {
            break;
        }
        if listed[earlier].0 < depth {
            line.push(earlier);
            depth = listed[earlier].0;
        }
    }
    line
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
    spec.title = title(&task);
    spec.body = details(&task);
    spec.body.push_str("\nIts conversation, the latest last:\n");
    let agent = clip(&task.agent, AGENT_CHARS);
    let conversation = super::transcript::read(kit, &task.room, &agent, organization);
    spec.body.push_str(&conversation.await);
    if task.running() {
        let description = "Cancel this task; its agent stops working on it";
        spec.actions
            .push(action(open, "stop", "Stop task", description));
    }
    spec
}

fn title(task: &Task) -> String {
    format!(
        "Task for {} · {}",
        clip(&task.agent, AGENT_CHARS),
        task.state()
    )
}

fn details(task: &Task) -> String {
    let mut input = cut(&task.input, INPUT_BYTES).to_owned();
    if input.len() < task.input.len() {
        input.push_str("… (cut)");
    }
    format!(
        "Agent: {}\nState: {}\nConversation: {}\n\nTask:\n{input}\n",
        clip(&task.agent, AGENT_CHARS),
        task.summary(),
        task.room,
    )
}

/// The sidebar's list of `room`'s tasks: those that run, then the latest
/// ended.
pub(super) fn sidebar(tasks: &Tasks, room: &str) -> SidebarSection {
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
                .with_detail(task.summary())
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
    use std::time::{Duration, Instant};

    use super::super::Outcome;
    use super::*;

    fn task(id: &str, ended: bool) -> Task {
        Task {
            id: id.into(),
            parent: "main".into(),
            room: format!("main::{id}"),
            agent: "Review".into(),
            agent_id: "review".into(),
            input: format!("Task {id}"),
            started: Instant::now(),
            ended: ended.then_some((Duration::from_secs(3), Outcome::Completed)),
            spent: None,
        }
    }

    fn open() -> Open {
        Open {
            id: "p1".into(),
            room: "main".into(),
            agent: "code".into(),
            selected: None,
        }
    }

    fn listed(tasks: &Tasks) -> PanelSpec {
        list(PanelSpec::new("p1", COMMAND, "", ""), tasks, &open())
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
        let spec = listed(&tasks);
        assert_eq!(spec.actions.len(), LISTED);
        let running = spec.actions.iter().filter(|a| a.value.contains(":open:r"));
        assert_eq!(running.count(), 15);
        assert!(spec.body.contains("\n5 more tasks are not listed.\n"));
    }

    /// A running task handed on from an old ended one is listed under it,
    /// whatever ended since.
    #[test]
    fn a_running_task_is_listed_under_the_task_it_was_handed_on_from() {
        let tasks = Tasks::default();
        tasks.started(task("parent", false));
        let mut child = task("child", false);
        child.parent = "main::parent".into();
        child.room = "main::parent::child".into();
        tasks.started(child);
        tasks.ended(task("parent", true));
        for n in 0..10 {
            tasks.started(task(&format!("r{n}"), false));
        }
        for n in 0..LISTED {
            tasks.ended(task(&format!("e{n}"), true));
        }
        // The parent is the oldest ended task, past the list's cut.
        assert!(tasks.of("main").len() > LISTED);
        let spec = listed(&tasks);
        assert_eq!(spec.actions[0].value, "p1:open:parent");
        assert_eq!(spec.actions[1].value, "p1:open:child");
        assert!(spec.actions[1].label.starts_with("  Review · running"));
        assert_eq!(spec.actions.len(), LISTED);
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
        assert!(title(&long).len() <= 256);
        let tasks = Tasks::default();
        tasks.started(long);
        assert!(listed(&tasks).actions[0].label.len() <= 256);
    }
}
