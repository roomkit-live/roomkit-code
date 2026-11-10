//! What the launch tells a person watching it, on scripted host events.

use roomkit_tui::protocol::{Actor, Choice, ModeState, ModelInfo, ToolCall, ToolState};
use serde_json::json;

use super::*;

fn progress() -> Progress {
    Progress::new(Path::new("/work/project"), Some("Local".into()))
}

fn snapshot(mode: &str) -> Event {
    let mut snapshot = Snapshot::new("room", "Title", Actor::new("agent", "Agent"));
    snapshot.model = Some(ModelInfo::new("Engine", "qwen"));
    snapshot.mode = Some(ModeState::new("mode", mode, vec![Choice::new(mode, mode)]));
    Event::Snapshot(snapshot)
}

fn item(run: &str, call: ToolCall) -> Event {
    Event::Item {
        conversation: "room".into(),
        item: Item::Tool {
            actor: "agent".into(),
            run: run.into(),
            call,
        },
    }
}

fn shell(state: ToolState) -> ToolCall {
    ToolCall::new("c1", "shell", json!({"command": "cargo test"}), state)
}

fn timed(state: ToolState, duration_ms: u64) -> Event {
    item("run", shell(state).with_duration_ms(duration_ms))
}

/// A progress that already told the header's blank line and the call's start.
fn running() -> Progress {
    let mut progress = progress();
    progress.on_event(&item("run", shell(ToolState::Running)));
    progress
}

#[test]
fn the_first_snapshot_says_where_and_how_the_launch_runs() {
    let mut progress = progress();
    assert_eq!(
        progress.on_event(&snapshot("yolo")),
        [
            "roomkit-code · /work/project",
            "model: qwen · provider: Local · mode: yolo · session: room",
        ]
    );
    // A later snapshot is a refreshed view.
    assert!(progress.on_event(&snapshot("yolo")).is_empty());
}

#[test]
fn a_mode_that_asks_says_nobody_will_answer() {
    for mode in ["ask", "plan"] {
        let lines = self::progress().on_event(&snapshot(mode));
        let expected = format!("mode: {mode} (approvals refused)");
        assert!(lines[1].contains(&expected), "{lines:?}");
    }
}

#[test]
fn the_header_leaves_out_what_the_host_does_not_state() {
    let mut progress = Progress::new(Path::new("/work"), None);
    let bare = Snapshot::new("room", "Title", Actor::new("agent", "Agent"));
    assert_eq!(
        progress.on_event(&Event::Snapshot(bare)),
        ["roomkit-code · /work", "session: room"]
    );
}

#[test]
fn no_control_character_reaches_the_header() {
    let workspace = Path::new("/work/\u{1b}]0;x\u{7}dir");
    let mut progress = Progress::new(workspace, Some("Lo\u{1b}[2Jcal".into()));
    let lines = progress.on_event(&snapshot("yolo"));
    assert_eq!(lines[0], "roomkit-code · /work/]0;xdir");
    assert!(lines[1].contains("provider: Lo[2Jcal ·"), "{lines:?}");
}

#[test]
fn a_call_shows_when_it_starts_and_only_once() {
    let mut progress = progress();
    let start = item("run", shell(ToolState::Running));
    assert_eq!(progress.on_event(&start), ["", "  Run cargo test"]);
    assert!(progress.on_event(&start).is_empty());
    // Only the first call is set apart.
    let other = ToolCall::new("c2", "read", json!({"path": "a.rs"}), ToolState::Running);
    assert_eq!(progress.on_event(&item("run", other)), ["  Read a.rs"]);
}

#[test]
fn a_pending_call_has_not_started() {
    assert!(
        progress()
            .on_event(&item("run", shell(ToolState::Pending)))
            .is_empty()
    );
}

#[test]
fn a_call_id_is_scoped_to_its_run() {
    let mut progress = running();
    assert_eq!(
        progress.on_event(&item("other", shell(ToolState::Running))),
        ["  Run cargo test"]
    );
}

#[test]
fn a_quick_success_adds_nothing() {
    assert!(
        running()
            .on_event(&timed(ToolState::Succeeded, 2_999))
            .is_empty()
    );
}

#[test]
fn a_long_success_says_how_long() {
    assert_eq!(
        running().on_event(&timed(ToolState::Succeeded, 12_000)),
        ["  Run cargo test · 12s"]
    );
}

#[test]
fn an_end_shows_once_however_often_the_host_sends_it() {
    let mut progress = running();
    let end = timed(ToolState::Failed, 1_500);
    assert_eq!(progress.on_event(&end), ["  Run cargo test · failed · 1s"]);
    assert!(progress.on_event(&end).is_empty());
}

#[test]
fn a_failure_under_a_second_gives_no_duration() {
    assert_eq!(
        running().on_event(&timed(ToolState::Failed, 500)),
        ["  Run cargo test · failed"]
    );
}

#[test]
fn what_went_wrong_says_so() {
    let cancelled = running().on_event(&item("run", shell(ToolState::Cancelled)));
    assert_eq!(cancelled, ["  Run cargo test · cancelled"]);
    let refused = running().on_event(&item("run", shell(ToolState::Denied)));
    assert_eq!(refused, ["  Run cargo test · refused"]);
}

#[test]
fn a_call_first_seen_settled_still_shows_once() {
    let mut progress = progress();
    let quick = timed(ToolState::Succeeded, 10);
    assert_eq!(progress.on_event(&quick), ["", "  Run cargo test"]);
    assert!(progress.on_event(&quick).is_empty());
    let refused = ToolCall::new("c2", "write", json!({"path": "a"}), ToolState::Denied);
    assert_eq!(
        progress.on_event(&item("run", refused)),
        ["  Write a · refused"]
    );
}

#[test]
fn no_control_character_from_the_model_reaches_the_terminal() {
    let call = ToolCall::new(
        "c1",
        "shell",
        json!({"command": "ls\u{1b}[2J\r\nrm -rf /tmp/x\u{7}"}),
        ToolState::Running,
    );
    let lines = progress().on_event(&item("run", call));
    assert_eq!(lines[1], "  Run ls [2J rm -rf /tmp/x");
}

#[test]
fn other_events_add_nothing() {
    let notice = Event::Notice {
        conversation: "room".into(),
        text: "A notice".into(),
    };
    assert!(progress().on_event(&notice).is_empty());
}
