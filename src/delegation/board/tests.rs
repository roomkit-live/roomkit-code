#![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
use super::task::{TASK_PREVIEW, preview, seconds};
use super::*;

fn task(id: &str, parent: &str, room: &str) -> Task {
    Task {
        id: id.into(),
        parent: parent.into(),
        room: room.into(),
        agent: "Review".into(),
        agent_id: "review".into(),
        input: format!("Task {id}").into(),
        started: Instant::now(),
        ended: None,
        spent: None,
    }
}

fn ended(mut task: Task, outcome: Outcome) -> Task {
    task.ended = Some((Duration::from_millis(58_400), outcome));
    task
}

#[test]
fn consultation_only_reaches_a_running_sibling_on_the_same_channel() {
    let tasks = Tasks::default();
    let caller = task("caller", "parent", "caller-room");
    let mut other_agent = task("local", "parent", "local-room");
    other_agent.agent_id = "other-agent".into();
    for task in [
        caller.clone(),
        task("peer", "parent", "peer-room"),
        task("unrelated", "another-parent", "other-room"),
        task("nested", "peer-room", "nested-room"),
        other_agent,
    ] {
        tasks.started(task);
    }
    let peer = tasks.running_peer(&caller, "peer").unwrap();
    for id in ["caller", "nested", "unrelated", "local", "unknown"] {
        assert!(tasks.running_peer(&caller, id).is_none());
    }
    tasks.ended(ended(peer, Outcome::Completed));
    assert!(tasks.running_peer(&caller, "peer").is_none());
}

#[tokio::test]
async fn each_task_ended_lately_keeps_its_line() {
    let tasks = Tasks::default();
    for id in ["a", "b"] {
        tasks.started(task(id, "main", &format!("main::{id}")));
    }
    let first = ended(task("a", "main", "main::a"), Outcome::Completed);
    tasks.finished(first, None).await;
    let second = ended(task("b", "main", "main::b"), Outcome::Failed);
    tasks.finished(second, Some("max_rounds")).await;

    let said: Vec<_> = tasks
        .task_lines("main")
        .into_iter()
        .filter_map(|line| match line {
            roomkit_tui::protocol::Item::Message(message) => Some(message.text),
            _ => None,
        })
        .collect();
    assert_eq!(
        said,
        [
            "Task for Review completed in 58 s",
            "Task for Review failed · it reached the limit of tool rounds a turn may run"
        ]
    );
    assert!(tasks.task_lines("elsewhere").is_empty());
}

#[test]
fn a_notice_quotes_a_task_on_one_short_line() {
    let long = "Review\n the diff ".repeat(20);
    let line = preview(&long);
    assert_eq!(line.chars().count(), TASK_PREVIEW);
    assert!(line.starts_with("Review the diff Review") && line.ends_with('…'));
    assert_eq!(preview("Review  the\ndiff"), "Review the diff");
    assert_eq!(seconds(Duration::from_millis(1_400)), 1);
    assert_eq!(seconds(Duration::from_millis(200)), 1);
}

/// A conversation lists its tasks and those their workers handed on, each
/// under the task that handed it on; another conversation's stay out.
#[test]
fn a_conversation_lists_its_tasks_and_those_handed_on() {
    let tasks = Tasks::default();
    tasks.started(task("a", "main", "main::a"));
    tasks.started(task("other", "second", "second::x"));
    tasks.started(task("b", "main", "main::b"));
    tasks.started(task("a1", "main::a", "main::a::a1"));
    let listed: Vec<(usize, String)> = tasks
        .of("main")
        .into_iter()
        .map(|(depth, task)| (depth, task.id))
        .collect();
    assert_eq!(listed, [(0, "a".into()), (1, "a1".into()), (0, "b".into())]);
    assert!(tasks.find("main", "a1").is_some());
    assert!(tasks.find("main", "other").is_none());
    assert_eq!(tasks.running(), 4);
}

/// A task's end replaces its state and leaves a notice; an end told before
/// its start records the task, and the late start leaves it ended.
#[test]
fn a_task_ends_once_whichever_hook_is_told_first() {
    let tasks = Tasks::default();
    tasks.started(task("a", "main", "main::a"));
    assert_eq!(tasks.notices("main"), ["Task for Review started: Task a"]);
    tasks.ended(ended(task("a", "main", "main::a"), Outcome::Completed));
    assert_eq!(tasks.notices("main"), ["Task for Review completed in 58 s"]);
    let a = tasks.find("main", "a").unwrap();
    assert_eq!(a.state(), "completed in 58 s");

    let mut told = ended(task("quick", "main", "main::q"), Outcome::Cancelled);
    told.input = "".into();
    tasks.ended(told);
    tasks.started(task("quick", "main", "main::q"));
    assert_eq!(tasks.notices("main"), ["Task for Review cancelled"]);
    let quick = tasks.find("main", "quick").unwrap();
    assert_eq!(quick.state(), "cancelled after 58 s");
    assert_eq!(&*quick.input, "Task quick");
    assert_eq!(tasks.running(), 0);
}

/// A room says how its task ended; a status that is no end means the
/// launch that ran it could not say so.
#[test]
fn a_room_s_status_reads_as_how_its_task_ended() {
    assert_eq!(Outcome::recorded("completed"), Outcome::Completed);
    assert_eq!(Outcome::recorded("failed"), Outcome::Failed);
    assert_eq!(Outcome::recorded("cancelled"), Outcome::Cancelled);
    assert_eq!(Outcome::recorded("in_progress"), Outcome::Interrupted);
    assert_eq!(Outcome::recorded("pending"), Outcome::Interrupted);
}

/// What the hooks told stands: a task read back from its room adds only
/// what they did not.
#[test]
fn a_restored_task_never_replaces_one_the_hooks_told() {
    let tasks = Tasks::default();
    tasks.started(task("live", "main", "main::l"));
    tasks.restored(vec![
        ended(task("live", "main", "main::l"), Outcome::Failed),
        ended(task("old", "main", "main::o"), Outcome::Completed),
    ]);
    let listed: Vec<(String, bool)> = tasks
        .of("main")
        .into_iter()
        .map(|(_, task)| (task.id.clone(), task.running()))
        .collect();
    assert_eq!(listed, [("live".into(), true), ("old".into(), false)]);
    assert!(tasks.restoring("main"));
    assert!(!tasks.restoring("main"));
}

/// Whichever of its rooms a view shows, a conversation lists the same
/// tasks, and knows the agent to show when returning to it.
#[test]
fn a_task_s_room_belongs_to_its_conversation() {
    let tasks = Tasks::default();
    tasks.started(task("a", "main", "main::a"));
    tasks.started(task("a1", "main::a", "main::a::a1"));
    tasks.started(task("other", "second", "second::x"));
    assert_eq!(tasks.root_of("main::a::a1"), "main");
    assert_eq!(tasks.root_of("main"), "main");
    assert_eq!(tasks.root_of("unknown"), "unknown");
    let ids =
        |room: &str| -> Vec<String> { tasks.family(room).into_iter().map(|(_, t)| t.id).collect() };
    assert_eq!(ids("main::a"), ["a", "a1"]);
    assert_eq!(ids("main"), ["a", "a1"]);
    assert!(tasks.find("main::a", "a1").is_some());
    assert!(tasks.find("main::a", "other").is_none());

    tasks.viewing("main", "code");
    assert_eq!(
        tasks.conversation_of("main::a", "review"),
        ("main".to_owned(), "code".to_owned())
    );
    assert_eq!(
        tasks.conversation_of("second", "review"),
        ("second".to_owned(), "review".to_owned())
    );
}

/// A turn recorded in a task's room, whoever started it, is read again.
#[test]
fn a_task_s_figures_are_read_again_once_a_turn_is_recorded_there() {
    let spent = || super::Spent {
        usage: crate::exit_summary::Usage::for_tests(10, 1),
        cost: None,
    };
    let tasks = Tasks::default();
    tasks.ended(ended(task("a", "main", "main::a"), Outcome::Completed));
    tasks.ended(ended(task("b", "main", "main::b"), Outcome::Completed));
    tasks.keep_spent("a", spent(), tasks.reading("main::a"));
    tasks.keep_spent("b", spent(), tasks.reading("main::b"));
    let read = |id: &str| tasks.find("main", id).unwrap().spent.is_some();
    let before = tasks.version();
    tasks.consumed("main::a");
    assert!(!read("a") && read("b"));
    assert!(
        tasks.version() > before,
        "The conversation's figures changed"
    );
}

/// A reading begun before a turn was recorded misses that turn: it is
/// not kept, and the next view reads the room again.
#[test]
fn a_task_s_figures_read_before_a_new_turn_are_not_kept() {
    let tasks = Tasks::default();
    let spent = || super::Spent {
        usage: crate::exit_summary::Usage::for_tests(10, 1),
        cost: None,
    };
    tasks.ended(ended(task("a", "main", "main::a"), Outcome::Completed));
    tasks.ended(ended(task("b", "main", "main::b"), Outcome::Completed));
    let (a, b) = (tasks.reading("main::a"), tasks.reading("main::b"));
    tasks.consumed("main::a");
    tasks.keep_spent("a", spent(), a);
    tasks.keep_spent("b", spent(), b);
    let read = |id: &str| tasks.find("main", id).unwrap().spent.is_some();
    // A turn in another task's room leaves this reading as it is.
    assert!(!read("a") && read("b"));
}

/// A read that failed is tried again by the conversation's next view.
#[test]
fn a_conversation_whose_tasks_could_not_be_read_is_read_again() {
    let tasks = Tasks::default();
    assert!(tasks.restoring("main"));
    tasks.unread("main");
    assert!(tasks.restoring("main"));
    assert!(!tasks.restoring("main"));
}

/// An agent at work keeps the assembled agent its turn runs in.
#[test]
fn an_agent_on_a_task_is_known_until_the_task_ends() {
    let tasks = Tasks::default();
    tasks.started(task("a", "main", "main::a"));
    assert!(tasks.agent_running("review") && !tasks.agent_running("code"));
    tasks.ended(ended(task("a", "main", "main::a"), Outcome::Completed));
    assert!(!tasks.agent_running("review"));
}
