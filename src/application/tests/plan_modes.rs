//! An agent asks to enter Plan and, with a written plan, to leave it for Ask;
//! the user approves each, and approves the stored plan, not a summary.
use super::*;
use crate::permissions::Mode;
use roomkit_tui::protocol::{Decision, Permission};

fn plan() -> Value {
    json!({"title":"Refactor","objective":"Split the module","steps":[
        {"step_id":"inspect","description":"Inspect the project","status":"completed"},
        {"step_id":"split","description":"Split the module","status":"pending"}
    ]})
}

/// The next approval card; the modes the view was told meanwhile.
async fn approval_noting(client: &roomkit_tui::Client, modes: &mut Vec<String>) -> Permission {
    loop {
        match event(client).await {
            Event::PermissionRequested { permission, .. } => return permission,
            Event::ModeChanged { mode, .. } => modes.push(mode.selected),
            _ => {}
        }
    }
}

/// Events until the run ends; the modes the view was told meanwhile.
async fn finish_noting(client: &roomkit_tui::Client, modes: &mut Vec<String>) -> Vec<Permission> {
    let mut asked = Vec::new();
    loop {
        match event(client).await {
            Event::RunFinished { .. } => return asked,
            Event::PermissionRequested { permission, .. } => asked.push(permission),
            Event::ModeChanged { mode, .. } => modes.push(mode.selected),
            _ => {}
        }
    }
}

fn decide(client: &roomkit_tui::Client, id: u64, permission: &Permission, decision: Decision) {
    command(
        client,
        id,
        "main",
        Command::Decide {
            permission: permission.id.clone(),
            decision,
        },
    );
}

#[tokio::test]
async fn an_agent_asks_to_plan_and_implements_only_an_approved_plan() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![
        (
            "plan_mode_enter",
            json!({"reason":"The change spans several files"}),
        ),
        ("plan_create", plan()),
        ("plan_mode_exit", json!({})),
        ("write", json!({"path":"after.txt","content":"done"})),
    ])
    .await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(
        app.session()
            .run_with_commands(host, app.router(&settings.workspace)),
    );
    let Event::Snapshot(initial) = event(&client).await else {
        panic!("Expected snapshot");
    };
    assert_eq!(initial.mode.unwrap().selected, "ask");
    command(
        &client,
        1,
        "main",
        Command::Submit {
            agent: "code".into(),
            text: "Refactor the project".into(),
        },
    );
    let mut modes = Vec::new();
    let enter = approval_noting(&client, &mut modes).await;
    assert!(
        enter.detail.contains("moves to Plan") && enter.detail.contains("several files"),
        "{}",
        enter.detail
    );
    decide(&client, 2, &enter, Decision::Allow);
    // Leaving shows the stored plan, not a text the agent supplied.
    let exit = approval_noting(&client, &mut modes).await;
    assert_eq!(app.policy.mode("main"), Mode::Plan);
    assert!(exit.title.contains("Approve this plan"), "{}", exit.title);
    assert!(
        exit.detail.contains("Plan · Refactor")
            && exit.detail.contains("1. Inspect the project (completed)")
            && exit.detail.contains("2. Split the module"),
        "{}",
        exit.detail
    );
    decide(&client, 3, &exit, Decision::Allow);
    // In Ask, implementing still asks for each change.
    let write = approval_noting(&client, &mut modes).await;
    assert_eq!(app.policy.mode("main"), Mode::Ask);
    assert!(write.detail.contains("after.txt"), "{}", write.detail);
    decide(&client, 4, &write, Decision::Deny);
    assert!(finish_noting(&client, &mut modes).await.is_empty());
    // The view was told Plan, then Ask, by the live updates.
    let plan_at = modes.iter().position(|mode| mode == "plan").unwrap();
    assert!(
        modes[plan_at..].iter().any(|mode| mode == "ask"),
        "{modes:?}"
    );
    assert!(!root.path().join("after.txt").exists());
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}

#[tokio::test]
async fn a_refused_plan_keeps_plan_and_leaving_outside_plan_asks_nothing() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![
        // Outside Plan, leaving it is refused without a card.
        ("plan_mode_exit", json!({})),
        ("plan_mode_enter", json!({"reason":"Unclear scope"})),
        ("plan_create", plan()),
        ("plan_mode_exit", json!({})),
    ])
    .await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(
        app.session()
            .run_with_commands(host, app.router(&settings.workspace)),
    );
    assert!(matches!(event(&client).await, Event::Snapshot(_)));
    command(
        &client,
        1,
        "main",
        Command::Submit {
            agent: "code".into(),
            text: "Change something".into(),
        },
    );
    let mut modes = Vec::new();
    let enter = approval_noting(&client, &mut modes).await;
    assert!(enter.detail.contains("Unclear scope"), "{}", enter.detail);
    decide(&client, 2, &enter, Decision::Allow);
    let exit = approval_noting(&client, &mut modes).await;
    assert!(exit.title.contains("Approve this plan"));
    decide(&client, 3, &exit, Decision::Deny);
    assert!(finish_noting(&client, &mut modes).await.is_empty());
    assert_eq!(app.policy.mode("main"), Mode::Plan);
    let requests = mock.requests.lock().unwrap().clone();
    let results = serde_json::to_string(&requests).unwrap();
    assert!(results.contains("applies in Plan mode only"), "{results}");
    assert!(results.contains("The plan was not approved"), "{results}");
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}

/// An application running `calls` from `mode`, attached like the terminal,
/// with "Refactor the project" submitted.
async fn running(
    calls: Vec<(&'static str, Value)>,
    mode: Mode,
) -> (
    Application,
    roomkit_tui::Client,
    tokio::task::JoinHandle<std::result::Result<(), roomkit_tui_engine::AdapterError>>,
    Mock,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(calls).await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.mode = mode;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(
        app.session()
            .run_with_commands(host, app.router(&settings.workspace)),
    );
    assert!(matches!(event(&client).await, Event::Snapshot(_)));
    command(
        &client,
        1,
        "main",
        Command::Submit {
            agent: "code".into(),
            text: "Refactor the project".into(),
        },
    );
    (app, client, task, mock, root, state)
}

fn results(mock: &Mock) -> String {
    serde_json::to_string(&*mock.requests.lock().unwrap()).unwrap()
}

#[tokio::test]
async fn a_plan_changed_while_its_approval_is_pending_is_not_approved() {
    use roomkit_tools_plans::{PlanBackend, PlanScope, PlanUpdateInput};
    let calls = vec![
        ("plan_mode_enter", json!({"reason":"Several files"})),
        ("plan_create", plan()),
        ("plan_mode_exit", json!({})),
    ];
    let (app, client, task, mock, _root, _state) = running(calls, Mode::Ask).await;
    let mut modes = Vec::new();
    let enter = approval_noting(&client, &mut modes).await;
    decide(&client, 2, &enter, Decision::Allow);
    let exit = approval_noting(&client, &mut modes).await;
    assert!(exit.detail.contains("Plan · Refactor"), "{}", exit.detail);
    // Another call of the same round rewrites the plan behind the card.
    let scope = PlanScope {
        room_id: "main".into(),
        caller_id: "code".into(),
    };
    let shown = app
        .profiles
        .resources
        .plans
        .plan(&scope, None)
        .await
        .unwrap();
    let swap: PlanUpdateInput = serde_json::from_value(json!({
        "plan_id": shown.plan_id, "expected_revision": shown.revision, "title": "Swapped"
    }))
    .unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    app.profiles
        .resources
        .plans
        .update(&scope, &swap, &cancel)
        .await
        .unwrap();
    decide(&client, 3, &exit, Decision::Allow);
    assert!(finish_noting(&client, &mut modes).await.is_empty());
    assert_eq!(app.policy.mode("main"), Mode::Plan);
    assert!(results(&mock).contains("The plan changed while the user was approving it"));
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}

#[tokio::test]
async fn a_mode_switched_while_the_plan_awaits_approval_is_kept() {
    let calls = vec![
        ("plan_mode_enter", json!({"reason":"Several files"})),
        ("plan_create", plan()),
        ("plan_mode_exit", json!({})),
    ];
    let (app, client, task, mock, _root, _state) = running(calls, Mode::Ask).await;
    let mut modes = Vec::new();
    let enter = approval_noting(&client, &mut modes).await;
    decide(&client, 2, &enter, Decision::Allow);
    let exit = approval_noting(&client, &mut modes).await;
    // The user switches to Yolo before answering: the plan is not approved
    // into Ask, and the user's mode stays.
    command(
        &client,
        3,
        "main",
        Command::Invoke {
            name: "mode".into(),
            arguments: "yolo".into(),
        },
    );
    while app.policy.mode("main") != Mode::Yolo {
        let _ = event(&client).await;
    }
    decide(&client, 4, &exit, Decision::Allow);
    finish_noting(&client, &mut modes).await;
    assert_eq!(app.policy.mode("main"), Mode::Yolo);
    assert!(results(&mock).contains("The mode changed while approval was pending"));
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}

#[tokio::test]
async fn in_yolo_entering_plan_needs_no_card_but_leaving_still_does() {
    let calls = vec![
        ("plan_mode_enter", json!({"reason":"Several files"})),
        ("plan_create", plan()),
        ("plan_mode_exit", json!({})),
    ];
    let (app, client, task, _mock, _root, _state) = running(calls, Mode::Yolo).await;
    let mut modes = Vec::new();
    // The first card is the plan's: entering ran at once.
    let exit = approval_noting(&client, &mut modes).await;
    assert!(exit.title.contains("Approve this plan"), "{}", exit.title);
    assert_eq!(app.policy.mode("main"), Mode::Plan);
    decide(&client, 2, &exit, Decision::Allow);
    finish_noting(&client, &mut modes).await;
    // Approving moves to Ask, never back to Yolo.
    assert_eq!(app.policy.mode("main"), Mode::Ask);
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}
