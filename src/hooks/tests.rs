#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Invalid fixtures fail the test"
)]
use super::*;

#[test]
fn a_reviewed_command_shows_its_control_characters() {
    assert_eq!(reviewed("echo ok\n\tdone", 100), "echo ok\n\tdone");
    assert_eq!(
        reviewed("echo safe\u{1b}[2K\rrm -rf .", 100),
        "echo safe\\u{1b}[2K\\u{d}rm -rf ."
    );
    assert_eq!(reviewed(&"é".repeat(10), 5), "éé");
}
use clap::Parser;

fn answered(result: Value, copy: Value) -> ToolCallEvent {
    let mut event = ToolCallEvent::new(
        roomkit::model::ChannelId::new("code").unwrap(),
        roomkit::model::ChannelType::AI,
        "call-1",
        "read",
        Default::default(),
    );
    event.result = Some(result);
    event.structured_content = Some(copy);
    event
}

#[test]
fn a_hook_reads_a_tool_s_data_and_text_as_one_json_text() {
    let read =
        |response: Value| -> Value { serde_json::from_str(response.as_str().unwrap()).unwrap() };
    let plain = tool_response(&answered(json!("1: a\n"), json!({"data": {"n": 1}})));
    assert_eq!(read(plain), json!({"data": {"n": 1}, "text": "1: a\n"}));
    let nested = json!({"project_instructions": "rules", "text": "1: a\n"}).to_string();
    let ruled = tool_response(&answered(json!(nested), json!({"data": 1})));
    assert_eq!(
        read(ruled),
        json!({"data": 1, "text": "1: a\n", "project_instructions": "rules"})
    );
}

fn hooks(state: &std::path::Path, workspace: &std::path::Path) -> (Arc<Hooks>, PathBuf) {
    let cli = crate::config::Cli::try_parse_from([
        "roomkit-code",
        workspace.to_str().unwrap(),
        "--state-dir",
        state.to_str().unwrap(),
    ])
    .unwrap();
    let settings = crate::config::Settings::load(&cli).unwrap();
    let path = state.join("diagnostics.jsonl");
    let log = crate::diagnostics::Diagnostics::open(path.clone(), false).unwrap();
    let policy = crate::permissions::Policy::new(crate::permissions::Mode::Ask);
    (Hooks::load(&settings, state, policy, log), path)
}

fn turn(hooks: &Hooks, operation: &str, ending: bool) -> Key {
    let key: Key = ("room".into(), "agent".into(), operation.into());
    let turn = Arc::new(Turn {
        context: Context {
            payload: json!({}),
            entries: Vec::new(),
        },
        begun: AtomicU64::new(0),
        ending: AtomicBool::new(ending),
        posts: TaskTracker::new(),
    });
    hooks.state().turns.insert(key.clone(), turn);
    key
}

/// Close, failing rather than hanging if closing never returns.
async fn close(hooks: &Hooks, orphaned: Duration) {
    tokio::time::timeout(Duration::from_secs(5), hooks.close_within(orphaned))
        .await
        .expect("Closing never returned");
}

#[tokio::test]
async fn closing_gives_up_on_a_turn_the_engine_never_finished() {
    let (workspace, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (hooks, path) = hooks(state.path(), workspace.path());
    turn(&hooks, "orphan", false);
    close(&hooks, Duration::from_millis(50)).await;
    let records = std::fs::read_to_string(path).unwrap();
    assert!(
        records.contains("hooks_close_timeout") && records.contains("\"turns\":1"),
        "{records}"
    );
}

#[tokio::test]
async fn closing_waits_for_a_turn_the_engine_is_finishing() {
    let (workspace, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (hooks, path) = hooks(state.path(), workspace.path());
    let key = turn(&hooks, "ending", true);
    // Its TurnEnd hooks outlast the orphan wait, then the turn ends.
    let finisher = hooks.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        finisher.state().turns.remove(&key);
        finisher.changes.send_replace(());
    });
    let started = std::time::Instant::now();
    close(&hooks, Duration::from_millis(50)).await;
    assert!(started.elapsed() >= Duration::from_millis(300));
    let records = std::fs::read_to_string(path).unwrap();
    assert!(!records.contains("hooks_close_timeout"), "{records}");
}

/// A message written during a turn may get a turn of its own, which
/// begins before the engine reports the previous one finished: the
/// agent's tool calls belong to the turn begun last.
#[tokio::test]
async fn a_tool_call_belongs_to_the_agent_s_latest_begun_turn() {
    let (workspace, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (hooks, _) = hooks(state.path(), workspace.path());
    let lifecycle = hooks.lifecycle();
    let (room, agent) = (
        roomkit::model::RoomId::new("room").unwrap(),
        roomkit::model::ChannelId::new("agent").unwrap(),
    );
    let view = SessionConfig::new(room.clone(), agent.clone(), "person");
    let content = roomkit::model::TextContent::new("Go on").into();
    for operation in ["first", "second"] {
        let operation = OperationId::new(operation);
        lifecycle.admit(&view, &operation, &content).await.unwrap();
        lifecycle.started(&view, &operation).await;
    }
    let mut event = ToolCallEvent::new(
        agent,
        roomkit::model::ChannelType::AI,
        "call",
        "read",
        Default::default(),
    );
    event.room_id = Some(room);
    let second = ("room".into(), "agent".into(), "second".into());
    let second = hooks.state().turns.get(&second).cloned().unwrap();
    let found = hooks.tool_turn(&event).unwrap();
    assert!(
        Arc::ptr_eq(&found, &second),
        "The call went to the first turn"
    );
    close(&hooks, Duration::from_millis(50)).await;
}

/// The engine's tool event carries markers, no outcome: each marker
/// names the outcome it stands for, any other error reads `failed`.
#[test]
fn a_tool_outcome_is_read_from_the_event_markers() {
    let event = |is_error, refused, cancelled| {
        let mut event = ToolCallEvent::new(
            roomkit::model::ChannelId::new("code").unwrap(),
            roomkit::model::ChannelType::AI,
            "call",
            "read",
            Default::default(),
        );
        (event.is_error, event.refused, event.cancelled) = (is_error, refused, cancelled);
        event
    };
    let outcomes = [
        event(false, false, false),
        event(true, true, false),
        event(true, false, true),
        event(true, false, false),
    ]
    .map(|event| tool_outcome(&event));
    assert_eq!(outcomes, ["served", "refused", "cancelled", "failed"]);
}
