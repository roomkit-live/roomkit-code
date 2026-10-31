use super::*;
use roomkit_tools_plans::{PlanBackend, PlanCreateInput, PlanScope, PlanUpdateInput, StepStatus};
use roomkit_tui::protocol::{PanelSpec, SidebarContent};
use roomkit_tui_engine::{CommandHandler, CommandResult};
use tokio_util::sync::CancellationToken;

fn handler(app: &Application, workspace: &std::path::Path) -> crate::sessions::Sessions {
    crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: workspace.into(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
        services: app.services.clone(),
    }
}
fn panel(result: CommandResult) -> PanelSpec {
    let CommandResult::Events(events) = result else {
        panic!("Expected panel")
    };
    let Event::Panel { panel, .. } = events.into_iter().next().unwrap() else {
        panic!("Expected panel")
    };
    panel
}
fn create(title: &str) -> Value {
    json!({"title":title,"objective":"Deliver a checked change","steps":[
        {"step_id":"inspect","description":"Inspect the project","status":"completed"},
        {"step_id":"implement","description":"Implement the change","status":"pending"},
        {"step_id":"test","description":"Run the tests","status":"pending"}
    ]})
}

#[tokio::test]
async fn entering_plan_revokes_pending_mutations_but_web_still_asks() {
    use roomkit_tui::protocol::Decision;
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![
        ("write", json!({"path":"denied","content":"no"})),
        ("web_fetch", json!({"url":"https://example.org/fixture"})),
    ])
    .await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    event(&client).await;
    command(
        &client,
        1,
        "main",
        Command::Submit {
            agent: "code".into(),
            text: "Change the fixture".into(),
        },
    );
    let pending = approval(&client).await;
    app.policy.set("main", "plan").unwrap();
    command(
        &client,
        2,
        "main",
        Command::Decide {
            permission: pending.id,
            decision: Decision::Allow,
        },
    );
    let web = approval(&client).await;
    assert!(web.title.contains("web_fetch"));
    assert!(!root.path().join("denied").exists());
    command(
        &client,
        3,
        "main",
        Command::Decide {
            permission: web.id,
            decision: Decision::Deny,
        },
    );
    loop {
        if matches!(event(&client).await, Event::OperationFinished { .. }) {
            break;
        }
    }
    assert!(
        mock.requests.lock().unwrap()[1]
            .to_string()
            .contains("mode changed to Plan")
    );
    for name in [
        "mcp__server__read",
        "process_write",
        "process_stop",
        "output_forget",
        "future_tool",
    ] {
        let e = roomkit::ToolCallEvent::new(
            app.view.agent.clone(),
            app.view.room.clone(),
            roomkit::model::ToolCall::new("test", name, Default::default()),
        );
        assert!(
            matches!(
                app.policy.gate(&app.channel, e).await,
                roomkit::ToolUse::Block(_)
            ),
            "{name}"
        );
    }
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}

#[tokio::test]
async fn plan_mode_uses_native_tools_blocks_effects_and_resumes_the_same_plan_in_yolo() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("source"), "original").unwrap();
    let mock = Mock::start(vec![
        ("read", json!({"path":"source"})),
        ("plan_create", create("Project change")),
        ("write", json!({"path":"denied-write","content":"no"})),
        (
            "edit",
            json!({"path":"source","old_text":"original","new_text":"no"}),
        ),
        ("shell", json!({"command":"touch denied-shell"})),
        ("process_start", json!({"command":"touch denied-process"})),
        (
            "patch",
            json!({"changes":[{"op":"add","path":"denied-patch","content":"no"}],"dry_run":false}),
        ),
        ("plan_list", json!({})),
    ])
    .await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.mode = crate::permissions::Mode::Plan;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let session = turn(&app).await;
    let snapshot = session.snapshot().unwrap();
    let mut plan_id = None;
    for item in &snapshot.items {
        if let Item::Tool { call, .. } = item {
            if ["read", "plan_create", "plan_list"].contains(&call.name.as_str()) {
                assert_eq!(
                    call.state,
                    ToolState::Succeeded,
                    "{}: {}",
                    call.name,
                    call.text
                );
            } else {
                assert!(
                    call.text.contains("Plan mode"),
                    "{}: {}",
                    call.name,
                    call.text
                );
            }
            if call.name == "plan_create" {
                plan_id = serde_json::from_str::<Value>(&call.text).unwrap()["data"]["plan_id"]
                    .as_str()
                    .map(str::to_owned);
            }
        }
    }
    let id = plan_id.unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join("source")).unwrap(),
        "original"
    );
    for path in [
        "denied-write",
        "denied-shell",
        "denied-process",
        "denied-patch",
    ] {
        assert!(!root.path().join(path).exists());
    }
    let requests = mock.requests.lock().unwrap().clone();
    let first = &requests[0];
    let messages = first["messages"].as_array().unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|m| m["content"] == "Inspect the project.")
            .count(),
        1
    );
    assert!(
        messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("Application mode: Plan")
    );
    assert!(
        messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("[Instruction from the application:")
    );
    let shown = roomkit_tui::render::screen(
        &session,
        140,
        60,
        &crate::tool_views::renderers(root.path()),
    )
    .lines
    .join("\n");
    assert!(
        shown.contains("Project change") && shown.contains("1/3 completed"),
        "{shown}"
    );
    assert!(!shown.contains("\"expected_revision\""));
    let h = handler(&app, root.path());
    let manual = h
        .handle(
            Request::new(
                RequestId::new(100),
                "manual",
                "main",
                Command::Invoke {
                    name: "shell".into(),
                    arguments: "touch denied-manual".into(),
                },
            ),
            app.view.clone(),
        )
        .await;
    assert!(manual.unwrap_err().message.contains("Plan"));
    app.close().await.unwrap();
    drop(app);

    let second = Mock::start(vec![
        ("plan_read", json!({"plan_id":id})),
        ("write", json!({"path":"implemented","content":"done"})),
        (
            "plan_update",
            json!({"plan_id":id,"expected_revision":1,"steps":[
                {"step_id":"inspect","description":"Inspect the project","status":"completed"},
                {"step_id":"implement","description":"Implement the change","status":"completed"},
                {"step_id":"test","description":"Run the tests","status":"pending"}
            ]}),
        ),
    ])
    .await;
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let detail = panel(app.profiles.plans.handle(&app.view, &id).await.unwrap());
    assert!(detail.body.contains("1/3 completed"));
    let provider = saved_provider_id(&settings);
    edit_profile(
        &mut app,
        "providers",
        &format!("edit:{provider}"),
        &[("url", &second.base_url)],
    )
    .await;
    app.policy.set("main", "yolo").unwrap();
    assert!(
        second.requests.lock().unwrap().is_empty(),
        "A mode change must not execute the plan"
    );
    turn_operation(&app, "implement").await;
    assert_eq!(
        std::fs::read_to_string(root.path().join("implemented")).unwrap(),
        "done"
    );
    let detail = panel(app.profiles.plans.handle(&app.view, &id).await.unwrap());
    assert!(detail.body.contains("2/3 completed"));
    let second_requests = second.requests.lock().unwrap().clone();
    assert_eq!(first["tools"], second_requests[0]["tools"]);
    assert_eq!(
        first["messages"][0], second_requests[0]["messages"][0],
        "Mode changes must preserve the system prompt"
    );
    assert!(
        second_requests[0]["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("Application mode: Yolo")
    );
    drop(second_requests);
    app.close().await.unwrap();
}

#[tokio::test]
async fn plan_views_follow_live_records_bound_long_steps_and_reject_stale_actions() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    let plans = &app.profiles.resources.plans;
    let owner = PlanScope {
        room_id: "main".into(),
        caller_id: "code".into(),
    };
    let cancel = CancellationToken::new();
    let h = handler(&app, root.path());
    let mut events = h.subscribe(app.view.clone()).unwrap();
    let first = plans
        .create(
            &owner,
            &serde_json::from_value::<PlanCreateInput>(create("First")).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    let second = plans
        .create(
            &owner,
            &serde_json::from_value::<PlanCreateInput>(create("Second")).unwrap(),
            &cancel,
        )
        .await
        .unwrap();
    let list = panel(app.profiles.plans.handle(&app.view, "").await.unwrap());
    assert_eq!(list.actions.len(), 2);
    let choice = list.actions.iter().find(|a| a.label == "Second").unwrap();
    let detail = panel(
        app.profiles
            .plans
            .handle(&app.view, &choice.value)
            .await
            .unwrap(),
    );
    let follow = detail
        .actions
        .iter()
        .find(|a| a.label == "Follow in sidebar")
        .unwrap();
    let followed = panel(
        app.profiles
            .plans
            .handle(&app.view, &follow.value)
            .await
            .unwrap(),
    );
    assert!(
        app.profiles
            .plans
            .handle(&app.view, &follow.value)
            .await
            .is_err(),
        "Old panel actions cannot select or overwrite newer views"
    );
    let mut steps = second.steps.clone();
    steps[1].status = StepStatus::InProgress;
    steps[1].description = "A long Unicode step à vérifier ".repeat(40);
    steps[2].status = StepStatus::Blocked;
    plans
        .update(
            &owner,
            &PlanUpdateInput {
                plan_id: second.plan_id,
                expected_revision: 1,
                title: None,
                objective: None,
                steps: Some(steps),
            },
            &cancel,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let update = events.borrow_and_update().clone();
            if update.iter().any(|e| matches!(e, Event::PanelUpdated { panel, .. } if panel.id == followed.id && panel.body.contains("1 blocked"))) {
                let sidebar = update.iter().find_map(|e| if let Event::SidebarChanged { sidebar, .. } = e { sidebar.as_ref() } else { None }).unwrap();
                let section = sidebar.sections.iter().find(|s| s.title == "Plan").unwrap();
                let SidebarContent::Entries { entries } = &section.content else { panic!("Expected plan entries") };
                assert_eq!(entries[0].label, "Second");
                assert!(entries.iter().all(|e| e.label.len() <= 256 && e.detail.len() <= 1024));
                assert!(entries.iter().any(|e| e.detail.ends_with('…')));
                assert!(entries.iter().any(|e| e.detail.contains("Run the tests")));
                break;
            }
            events.changed().await.unwrap();
        }
    }).await.unwrap();
    let mut other = app.view.clone();
    other.agent = ChannelId::new("other").unwrap();
    assert!(
        app.profiles
            .plans
            .handle(&other, &first.plan_id)
            .await
            .is_err()
    );
    assert!(app.profiles.plans.updates(&other).await.is_empty());
    app.profiles
        .plans
        .handle(&app.view, &followed.dismiss)
        .await
        .unwrap();
    assert!(app.profiles.plans.updates(&app.view).await.is_empty());
    assert!(
        mock.requests.lock().unwrap().is_empty(),
        "Browsing plans never invokes the model"
    );
    drop(events);
    app.close().await.unwrap();
}
