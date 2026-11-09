use super::*;
use roomkit_tui::protocol::PanelSpec;
use roomkit_tui_engine::CommandResult;
use std::path::Path;

pub(super) fn configure(root: &Path, events: &[&str], body: &str, plan: bool) {
    std::fs::create_dir_all(root.join(".roomkit")).unwrap();
    std::fs::write(root.join("hook.py"), format!(
        "import json, sys\nfrom pathlib import Path\ne = json.load(sys.stdin)\nwith open('hook-events.jsonl', 'a') as f:\n    f.write(json.dumps(e) + '\\n')\n{body}\n"
    )).unwrap();
    let hooks: serde_json::Map<String, Value> = events.iter().map(|event| ((*event).into(), json!([{
        "hooks":[{"type":"command","command":"python3 hook.py","timeout":2,"statusMessage":format!("Fixture {event}"),"roomkit":{"run_in_plan":plan}}]
    }]))).collect();
    std::fs::write(
        root.join(".roomkit/hooks.json"),
        json!({"hooks":hooks}).to_string(),
    )
    .unwrap();
}

fn panel(result: CommandResult) -> PanelSpec {
    let CommandResult::Events(events) = result else {
        panic!("Expected panel")
    };
    events
        .into_iter()
        .find_map(|e| {
            if let Event::Panel { panel, .. } = e {
                Some(panel)
            } else {
                None
            }
        })
        .unwrap()
}

pub(super) fn enable_all(app: &Application) {
    let list = panel(app.services.hooks.panel(&app.view, "").unwrap());
    let ids: Vec<_> = list
        .actions
        .iter()
        .filter_map(|a| a.value.split_once(":open:").map(|(_, id)| id.to_string()))
        .collect();
    for id in ids {
        let list = panel(app.services.hooks.panel(&app.view, "").unwrap());
        let entry = panel(
            app.services
                .hooks
                .panel(&app.view, &format!("{}:open:{id}", list.id))
                .unwrap(),
        );
        let enabled = panel(
            app.services
                .hooks
                .panel(&app.view, &format!("{}:enable", entry.id))
                .unwrap(),
        );
        assert!(enabled.body.contains("State: enabled"));
    }
}

pub(super) fn records(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(root.join("hook-events.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}

#[tokio::test]
async fn hooks_span_real_tool_rounds_preserve_correlation_and_resume_without_replay() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    configure(
        root.path(),
        &[
            "SessionStart",
            "SessionEnd",
            "UserPromptSubmit",
            "TurnStart",
            "TurnEnd",
            "PreToolUse",
            "PostToolUse",
            "PostToolUseFailure",
        ],
        "if e['hook_event_name'] == 'PreToolUse' and e['tool_input'].get('path') == 'denied':\n    print(json.dumps({'hookSpecificOutput': {'hookEventName':'PreToolUse', 'permissionDecision':'deny', 'permissionDecisionReason':'Fixture policy'}}))",
        false,
    );
    std::fs::write(root.path().join("source"), "sample").unwrap();
    let mock = Mock::start(vec![
        ("read", json!({"path":"source"})),
        (
            "write",
            json!({"path":"denied","content":"must not happen"}),
        ),
    ])
    .await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.mode = crate::permissions::Mode::Yolo;
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert!(records(root.path()).is_empty());
    enable_all(&app);
    turn(&app).await;
    assert!(!root.path().join("denied").exists());
    let entries = records(root.path());
    let names: Vec<_> = entries
        .iter()
        .map(|e| e["hook_event_name"].as_str().unwrap())
        .collect();
    assert_eq!(names.first(), Some(&"SessionStart"));
    assert_eq!(names.last(), Some(&"TurnEnd"));
    for name in [
        "UserPromptSubmit",
        "TurnStart",
        "TurnEnd",
        "PostToolUse",
        "PostToolUseFailure",
    ] {
        assert_eq!(names.iter().filter(|n| **n == name).count(), 1, "{names:?}");
    }
    let denied = entries
        .iter()
        .find(|e| e["hook_event_name"] == "PostToolUseFailure")
        .unwrap();
    assert_eq!(denied["roomkit"]["outcome"], "refused");
    assert_eq!(denied["tool_use_id"], "call-1");
    assert!(
        entries
            .iter()
            .skip(1)
            .all(|e| e["roomkit"]["turn_id"] == "inspect-project")
    );
    assert!(
        entries
            .iter()
            .all(|e| e["session_id"] == "main" && e["roomkit"]["agent_id"] == "code")
    );
    assert_eq!(entries.last().unwrap()["roomkit"]["outcome"], "completed");
    let first_execution = entries[0]["roomkit"]["execution_id"].clone();
    let count = entries.len();
    // Same operation retried through a new view: no callback or tool is replayed.
    turn(&app).await;
    assert_eq!(records(root.path()).len(), count);
    app.close().await.unwrap();
    assert_eq!(
        records(root.path()).last().unwrap()["hook_event_name"],
        "SessionEnd"
    );
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    // Restoration alone is read-only; attaching opens one new execution.
    app.session().snapshot().await.unwrap();
    assert_eq!(records(root.path()).len(), count + 1);
    turn_operation(&app, "next-turn").await;
    let entries = records(root.path());
    assert_eq!(
        entries
            .iter()
            .filter(|e| e["hook_event_name"] == "SessionStart")
            .count(),
        2
    );
    assert_ne!(
        entries
            .iter()
            .rev()
            .find(|e| e["hook_event_name"] == "SessionStart")
            .unwrap()["roomkit"]["execution_id"],
        first_execution
    );
    app.close().await.unwrap();
    let project = crate::digest::sha256_hex(root.path().as_os_str().as_encoded_bytes());
    let log = std::fs::read_to_string(
        state
            .path()
            .join("projects")
            .join(project)
            .join("diagnostics.jsonl"),
    )
    .unwrap();
    assert!(!log.contains("must not happen"));
}

#[tokio::test]
async fn unreviewed_definitions_do_not_run_and_changed_definitions_lose_trust() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    configure(root.path(), &["TurnStart"], "", false);
    let mock = Mock::start(vec![]).await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    turn_operation(&app, "disabled").await;
    assert!(records(root.path()).is_empty());
    enable_all(&app);
    turn_operation(&app, "enabled").await;
    assert_eq!(records(root.path()).len(), 1);
    let list = panel(app.services.hooks.panel(&app.view, "").unwrap());
    let inspected = panel(
        app.services
            .hooks
            .panel(&app.view, &list.actions[1].value)
            .unwrap(),
    );
    let path = root.path().join(".roomkit/hooks.json");
    let changed = std::fs::read_to_string(&path)
        .unwrap()
        .replace("python3 hook.py", "python3 hook.py; true");
    std::fs::write(path, changed).unwrap();
    assert!(
        app.services
            .hooks
            .panel(&app.view, &format!("{}:enable", inspected.id))
            .is_err()
    );
    turn_operation(&app, "changed").await;
    assert_eq!(records(root.path()).len(), 1);
    app.close().await.unwrap();
}

#[tokio::test]
async fn hook_can_require_review_for_read_in_yolo_and_allow_cannot_bypass_ask() {
    for (mode, name, decision, args) in [
        (
            crate::permissions::Mode::Yolo,
            "read",
            "ask",
            json!({"path":"source"}),
        ),
        (
            crate::permissions::Mode::Ask,
            "write",
            "allow",
            json!({"path":"written","content":"fixture"}),
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("source"), "fixture").unwrap();
        configure(
            root.path(),
            &["PreToolUse"],
            &format!(
                "print(json.dumps({{'hookSpecificOutput':{{'hookEventName':'PreToolUse','permissionDecision':'{decision}','permissionDecisionReason':'Review fixture'}}}}))"
            ),
            false,
        );
        let mock = Mock::start(vec![(name, args)]).await;
        let mut settings = settings(root.path(), state.path(), &mock.base_url);
        settings.mode = mode;
        let app = Application::open(&settings, String::new()).await.unwrap();
        enable_all(&app);
        let (client, host) = connection(NonZeroUsize::new(256).unwrap());
        let task = tokio::spawn(app.session().run(host));
        event(&client).await;
        command(
            &client,
            1,
            "main",
            Command::Submit {
                agent: "code".into(),
                text: "Test hooks".into(),
            },
        );
        let pending = approval(&client).await;
        assert!(pending.title.contains(name));
        if decision == "ask" {
            assert!(pending.detail.contains("Review fixture"));
        }
        command(
            &client,
            2,
            "main",
            Command::Decide {
                permission: pending.id,
                decision: roomkit_tui::protocol::Decision::Deny,
            },
        );
        loop {
            if matches!(event(&client).await, Event::OperationFinished { .. }) {
                break;
            }
        }
        assert!(!root.path().join("written").exists());
        drop(client);
        task.await.unwrap().unwrap();
        app.close().await.unwrap();
    }
}

#[tokio::test]
async fn prompt_refusal_is_visible_and_does_not_start_a_model_turn() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    configure(
        root.path(),
        &["UserPromptSubmit", "TurnStart", "TurnEnd"],
        "if e['hook_event_name'] == 'UserPromptSubmit':\n    print(json.dumps({'decision':'block','reason':'Provide an issue identifier'}))",
        false,
    );
    let mock = Mock::start(vec![]).await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    enable_all(&app);
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    event(&client).await;
    command(
        &client,
        1,
        "main",
        Command::Submit {
            agent: "code".into(),
            text: "Rejected private fixture prompt".into(),
        },
    );
    let mut notice = false;
    loop {
        match event(&client).await {
            Event::Notice { text, .. } => notice |= text.contains("Provide an issue identifier"),
            Event::OperationFinished { outcome, .. } => {
                assert_eq!(outcome, RunOutcome::Failed);
                break;
            }
            _ => {}
        }
    }
    assert!(notice && mock.requests.lock().unwrap().is_empty());
    assert!(app.session().snapshot().await.unwrap().items.is_empty());
    assert_eq!(records(root.path()).len(), 1);
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}

#[tokio::test]
async fn plan_skips_commands_unless_explicit_and_still_blocks_mutations() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    configure(root.path(), &["PreToolUse", "TurnStart"], "", false);
    let mock = Mock::start(vec![("write", json!({"path":"denied","content":"no"}))]).await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.mode = crate::permissions::Mode::Plan;
    let app = Application::open(&settings, String::new()).await.unwrap();
    enable_all(&app);
    turn(&app).await;
    assert!(records(root.path()).is_empty());
    assert!(!root.path().join("denied").exists());
    configure(root.path(), &["TurnStart"], "", true);
    let list = panel(app.services.hooks.panel(&app.view, "").unwrap());
    app.services
        .hooks
        .panel(&app.view, &format!("{}:reload", list.id))
        .unwrap();
    enable_all(&app);
    turn_operation(&app, "plan-opt-in").await;
    assert_eq!(records(root.path()).len(), 1);
    app.close().await.unwrap();
}

#[tokio::test]
async fn interrupted_tool_hook_is_cleaned_up_and_emits_one_cancelled_turn_end() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    configure(
        root.path(),
        &["TurnStart", "TurnEnd", "PreToolUse", "PostToolUseFailure"],
        "if e['hook_event_name'] == 'PreToolUse':\n    import time\n    Path('hook-running').touch()\n    time.sleep(1.5)\n    Path('hook-leaked').touch()",
        false,
    );
    let mock = Mock::start(vec![("write", json!({"path":"denied","content":"no"}))]).await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.mode = crate::permissions::Mode::Yolo;
    let app = Application::open(&settings, String::new()).await.unwrap();
    enable_all(&app);
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    event(&client).await;
    command(
        &client,
        1,
        "main",
        Command::Submit {
            agent: "code".into(),
            text: "Test cancellation".into(),
        },
    );
    let run = loop {
        if let Event::RunStarted { run, .. } = event(&client).await
            && !run.as_str().starts_with("prepare:")
        {
            break run;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.path().join("hook-running").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    command(&client, 2, "main", Command::Interrupt { run });
    loop {
        if let Event::OperationFinished { outcome, .. } = event(&client).await {
            assert_eq!(outcome, RunOutcome::Interrupted);
            break;
        }
    }
    let entries = records(root.path());
    assert_eq!(
        entries
            .iter()
            .filter(|e| e["hook_event_name"] == "TurnEnd")
            .count(),
        1
    );
    assert_eq!(entries.last().unwrap()["hook_event_name"], "TurnEnd");
    assert_eq!(entries.last().unwrap()["roomkit"]["outcome"], "cancelled");
    drop(client);
    task.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(5), app.close())
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1600)).await;
    assert!(!root.path().join("denied").exists() && !root.path().join("hook-leaked").exists());
}

#[tokio::test]
async fn timeout_refuses_tool_but_post_hook_error_cannot_undo_a_successful_write() {
    for (event, script, written) in [
        ("PreToolUse", "import time\ntime.sleep(3)", false),
        (
            "PostToolUse",
            "print(json.dumps({'decision':'block','reason':'Cannot undo'}))",
            true,
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        configure(
            root.path(),
            &[event, "TurnEnd"],
            &format!(
                "if e['hook_event_name'] == '{event}':\n{}",
                script
                    .lines()
                    .map(|s| format!("    {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            false,
        );
        let mock = Mock::start(vec![(
            "write",
            json!({"path":"written","content":"fixture"}),
        )])
        .await;
        let mut settings = settings(root.path(), state.path(), &mock.base_url);
        settings.mode = crate::permissions::Mode::Yolo;
        let app = Application::open(&settings, String::new()).await.unwrap();
        enable_all(&app);
        turn(&app).await;
        assert_eq!(root.path().join("written").exists(), written);
        assert_eq!(
            records(root.path()).last().unwrap()["hook_event_name"],
            "TurnEnd"
        );
        let list = panel(app.services.hooks.panel(&app.view, "").unwrap());
        let hook = list
            .actions
            .iter()
            .find(|a| a.label == format!("Fixture {event}"))
            .unwrap();
        let detail = panel(app.services.hooks.panel(&app.view, &hook.value).unwrap());
        assert!(detail.body.contains("failed"));
        app.close().await.unwrap();
    }
}

#[tokio::test]
async fn global_and_project_hooks_are_additive_and_invalid_config_is_visible_before_admission() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    configure(root.path(), &["TurnStart"], "", false);
    std::fs::write(state.path().join("hooks.json"), json!({"hooks":{"TurnStart":[{"hooks":[{"type":"command","command":"python3 hook.py","timeout":2}]}]}}).to_string()).unwrap();
    let mock = Mock::start(vec![]).await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    enable_all(&app);
    turn(&app).await;
    assert_eq!(records(root.path()).len(), 2);
    let list = panel(app.services.hooks.panel(&app.view, "").unwrap());
    std::fs::write(
        root.path().join(".roomkit/hooks.json"),
        br#"{"hooks":{"Stop":[]}}"#,
    )
    .unwrap();
    let result = panel(
        app.services
            .hooks
            .panel(&app.view, &format!("{}:reload", list.id))
            .unwrap(),
    );
    assert!(result.body.contains("Invalid hook configuration"));
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    event(&client).await;
    command(
        &client,
        2,
        "main",
        Command::Submit {
            agent: "code".into(),
            text: "Must not run".into(),
        },
    );
    loop {
        if let Event::OperationFinished { outcome, .. } = event(&client).await {
            assert_eq!(outcome, RunOutcome::Failed);
            break;
        }
    }
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}

#[tokio::test]
async fn provider_failure_emits_one_failed_turn_end() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    configure(root.path(), &["TurnStart", "TurnEnd"], "", false);
    let mock = Mock::start(vec![]).await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    enable_all(&app);
    drop(mock);
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    event(&client).await;
    command(
        &client,
        1,
        "main",
        Command::Submit {
            agent: "code".into(),
            text: "Provider is offline".into(),
        },
    );
    loop {
        if let Event::OperationFinished { outcome, .. } = event(&client).await {
            assert_eq!(outcome, RunOutcome::Failed);
            break;
        }
    }
    let entries = records(root.path());
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1]["hook_event_name"], "TurnEnd");
    assert_eq!(entries[1]["roomkit"]["outcome"], "failed");
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}

#[tokio::test]
async fn changing_views_does_not_close_sessions_or_replay_their_start() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    configure(root.path(), &["SessionStart", "SessionEnd"], "", false);
    let mock = Mock::start(vec![]).await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    enable_all(&app);
    turn(&app).await;
    let h = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: root.path().into(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
        services: app.services.clone(),
    };
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run_with_commands(host, h));
    event(&client).await;
    command(
        &client,
        1,
        "main",
        Command::Invoke {
            name: "new".into(),
            arguments: String::new(),
        },
    );
    let other = loop {
        if let Event::Snapshot(snapshot) = event(&client).await {
            break snapshot.conversation.into_inner();
        }
    };
    assert_ne!(other, "main");
    command(
        &client,
        2,
        &other,
        Command::Invoke {
            name: "resume".into(),
            arguments: "main".into(),
        },
    );
    loop {
        if let Event::Snapshot(snapshot) = event(&client).await {
            assert_eq!(snapshot.conversation.as_str(), "main");
            break;
        }
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while records(root.path()).len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let entries = records(root.path());
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|e| e["hook_event_name"] == "SessionStart")
    );
    assert_eq!(entries[1]["source"], "new");
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
    assert_eq!(
        records(root.path())
            .iter()
            .filter(|e| e["hook_event_name"] == "SessionEnd")
            .count(),
        2
    );
}

/// Moving into a delegated task's room starts no session: it belongs to its
/// conversation's.
#[tokio::test]
async fn a_task_s_room_is_no_session_of_its_own() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    configure(root.path(), &["SessionStart", "SessionEnd"], "", false);
    let mock = Mock::start(vec![]).await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    enable_all(&app);
    let mut view = app.view.clone();
    view.room = roomkit::model::RoomId::new("main::task-1").unwrap();
    view.group = Some("main".into());
    app.services.hooks.lifecycle().attached(&view);
    tokio::time::sleep(Duration::from_millis(300)).await;
    app.close().await.unwrap();
    assert!(
        records(root.path()).is_empty(),
        "{:?}",
        records(root.path())
    );
}

#[tokio::test]
async fn shutdown_interrupts_detached_preparation_and_runs_session_end() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    configure(
        root.path(),
        &["UserPromptSubmit", "SessionEnd"],
        "if e['hook_event_name'] == 'UserPromptSubmit':\n    Path('preparing').touch()\n    import time\n    time.sleep(1.5)\n    Path('leaked').touch()",
        false,
    );
    let mock = Mock::start(vec![]).await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    enable_all(&app);
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    event(&client).await;
    command(
        &client,
        1,
        "main",
        Command::Submit {
            agent: "code".into(),
            text: "Wait for preparation".into(),
        },
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.path().join("preparing").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    drop(client);
    task.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(5), app.close())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        records(root.path()).last().unwrap()["hook_event_name"],
        "SessionEnd"
    );
    assert!(mock.requests.lock().unwrap().is_empty());
    tokio::time::sleep(Duration::from_millis(1600)).await;
    assert!(!root.path().join("leaked").exists());
}
