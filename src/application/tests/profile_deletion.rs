use super::*;
use roomkit_tui_engine::CommandResult;

async fn invoke(app: &Application, name: &str, arguments: &str) -> Result<CommandResult> {
    profile_command(
        app,
        Command::Invoke {
            name: name.into(),
            arguments: arguments.into(),
        },
    )
    .await
}

async fn confirmation(app: &Application, name: &str, id: &str) -> String {
    let CommandResult::Events(events) = invoke(app, name, &format!("delete {id}")).await.unwrap()
    else {
        panic!("Expected confirmation")
    };
    let Event::Choices { choices, .. } = &events[0] else {
        panic!("Expected choices")
    };
    assert_eq!(choices[0].label, "Cancel");
    assert_eq!(choices[1].label, "Delete");
    choices[1].value.clone()
}

async fn remove(app: &mut Application, name: &str, id: &str) {
    let token = confirmation(app, name, id).await;
    if let CommandResult::Switch(view) = invoke(app, name, &token).await.unwrap() {
        app.view = *view;
    }
}

async fn add_agent(app: &mut Application, id: &str, provider: &str) {
    edit_profile(
        app,
        "agents",
        "add",
        &[
            ("id", id),
            ("label", id),
            ("provider", provider),
            ("model", "fixture-model"),
            ("tools", ""),
        ],
    )
    .await;
}

#[tokio::test]
async fn deletion_handles_active_inactive_and_last_agents_without_erasing_history_or_reimporting() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let provider = saved_provider_id(&settings);
    let room = app.view.room.clone();
    turn(&app).await;
    add_agent(&mut app, "reviewer", &provider).await;
    add_agent(&mut app, "helper", &provider).await;
    let CommandResult::Events(events) = invoke(&app, "agents", "list").await.unwrap() else {
        panic!("Expected list")
    };
    let Event::Choices { choices, .. } = &events[0] else {
        panic!("Expected choices")
    };
    assert_eq!(
        choices
            .iter()
            .filter(|c| c.value.starts_with("open:"))
            .count(),
        3
    );
    assert!(
        choices
            .iter()
            .any(|c| c.value == "open:helper" && c.label.starts_with('●'))
    );

    remove(&mut app, "agents", "helper").await;
    assert_eq!(app.view.agent.as_str(), "code");
    assert!(app.view.submission_error.is_none());
    assert!(app.profiles.tools("helper").await.is_empty());
    remove(&mut app, "agents", "reviewer").await;
    assert_eq!(app.view.agent.as_str(), "code");
    turn_operation(&app, "remaining-agent").await;
    remove(&mut app, "agents", "code").await;
    assert!(app.view.model.is_none());
    assert!(app.view.submission_error.is_some());
    assert!(app.profiles.tools("code").await.is_empty());
    // A deleted agent leaves no breakdown of its requests behind.
    let (sidebar, _) = app.profiles.sidebar(&app.view).await;
    let context = sidebar
        .sections
        .iter()
        .find(|s| s.title == "Context")
        .unwrap();
    assert_eq!(
        context.content,
        roomkit_tui::protocol::SidebarContent::Context
    );
    assert_eq!(app.view.room, room);
    let history = app.session().snapshot().await.unwrap();
    assert!(
        history.items.iter().any(
            |item| matches!(item, Item::Message(m) if m.text == "Project inspection complete.")
        )
    );
    remove(&mut app, "providers", &provider).await;
    app.close().await.unwrap();
    drop(app);

    // Even the original launch URL/model must not recreate explicitly removed profiles.
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    assert!(app.view.model.is_none());
    assert!(app.view.submission_error.is_some());
    let saved: Value = serde_json::from_slice(
        &std::fs::read(settings.state_dir.join("connections.json")).unwrap(),
    )
    .unwrap();
    assert!(saved["profiles"].as_object().unwrap().is_empty());
    assert!(!app.session().snapshot().await.unwrap().items.is_empty());
    edit_profile(
        &mut app,
        "providers",
        "add",
        &[("id", "new"), ("url", &mock.base_url)],
    )
    .await;
    add_agent(&mut app, "code", "new").await;
    assert!(app.view.submission_error.is_none());
    turn_operation(&app, "recreated-agent").await;
    app.close().await.unwrap();
}

#[tokio::test]
async fn provider_deletion_rechecks_references_created_in_another_project_during_confirmation() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let a_settings = settings(first.path(), state.path(), &mock.base_url);
    let mut a = Application::open(&a_settings, "fixture-private-key".into())
        .await
        .unwrap();
    let provider = saved_provider_id(&a_settings);
    let mut b_settings = settings(second.path(), state.path(), &mock.base_url);
    b_settings.base_url.clear();
    b_settings.model.clear();
    let mut b = Application::open(&b_settings, String::new()).await.unwrap();
    add_agent(&mut b, "remote", &provider).await;
    assert!(
        invoke(&a, "providers", &format!("delete {provider}"))
            .await
            .unwrap_err()
            .to_string()
            .contains("used by 2 agent(s)")
    );
    remove(&mut a, "agents", "code").await;
    let error = invoke(&a, "providers", &format!("delete {provider}"))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("remote") && error.contains("used by 1"));
    remove(&mut b, "agents", "remote").await;
    let token = confirmation(&a, "providers", &provider).await;
    add_agent(&mut b, "new-reference", &provider).await;
    assert!(
        invoke(&a, "providers", &token)
            .await
            .unwrap_err()
            .to_string()
            .contains("new-reference")
    );
    remove(&mut b, "agents", "new-reference").await;
    remove(&mut a, "providers", &provider).await;
    let saved = std::fs::read_to_string(state.path().join("connections.json")).unwrap();
    assert!(!saved.contains("fixture-private-key"));
    a.close().await.unwrap();
    b.close().await.unwrap();
}

#[tokio::test]
async fn cancelled_stale_or_cross_room_confirmation_never_deletes_a_profile() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let token = confirmation(&app, "agents", "code").await;
    invoke(&app, "agents", "open:code").await.unwrap(); // Cancel is the default choice.
    assert!(invoke(&app, "agents", &token).await.is_err());
    let token = confirmation(&app, "agents", "code").await;
    let mut wrong = app.view.clone();
    wrong.room = roomkit::model::RoomId::new("other").unwrap();
    assert!(
        app.profiles
            .handle(
                &app.kit,
                app.channel.id(),
                Request::new(
                    RequestId::new(9),
                    "wrong-view",
                    "other",
                    Command::Invoke {
                        name: "agents".into(),
                        arguments: token
                    }
                ),
                wrong
            )
            .await
            .is_err()
    );
    let token = confirmation(&app, "agents", "code").await;
    let path = app._storage.database.with_file_name("agents.json");
    let mut changed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    changed["agents"]["code"]["label"] = "Renamed externally".into();
    std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(
        invoke(&app, "agents", &token)
            .await
            .unwrap_err()
            .to_string()
            .contains("Profiles changed")
    );
    turn_operation(&app, "after-rejected-deletion").await;
    app.close().await.unwrap();
}

#[tokio::test]
async fn deleting_a_chatgpt_connection_keeps_account_management_separate_and_never_uses_credentials()
 {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, "fixture-secret".into())
        .await
        .unwrap();
    let path = state.path().join("connections.json");
    let mut document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    document["profiles"]["oauth"] = json!({"id":"oauth","label":"Account provider","base_url":roomkit_auth::CHATGPT_API_ROOT,
        "authentication":{"kind":"chat_gpt","account":uuid::Uuid::new_v4().to_string()},"timeout_seconds":60});
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
    for action in ["list", "open:oauth"] {
        let CommandResult::Events(events) = invoke(&app, "providers", action).await.unwrap() else {
            panic!("Expected menu")
        };
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains("fixture-secret")
        );
    }
    remove(&mut app, "providers", "oauth").await;
    assert!(!state.path().join("auth/accounts.json").exists());
    assert!(mock.requests.lock().unwrap().is_empty());
    app.close().await.unwrap();
}

#[tokio::test]
async fn an_open_agent_form_cannot_restore_a_deleted_provider_reference_and_edits_share_a_lease() {
    use roomkit_tui::protocol::FormValues;
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let a_settings = settings(first.path(), state.path(), &mock.base_url);
    let mut a = Application::open(&a_settings, String::new()).await.unwrap();
    edit_profile(
        &mut a,
        "providers",
        "add",
        &[("id", "unused"), ("url", &mock.base_url)],
    )
    .await;
    let mut b_settings = settings(second.path(), state.path(), &mock.base_url);
    b_settings.base_url.clear();
    b_settings.model.clear();
    let b = Application::open(&b_settings, String::new()).await.unwrap();
    let CommandResult::Events(events) = invoke(&b, "agents", "add").await.unwrap() else {
        panic!("Expected form")
    };
    let Event::Form { form, .. } = &events[0] else {
        panic!("Expected form")
    };
    let mut values = form
        .fields
        .iter()
        .map(|f| (f.name.clone(), f.value.clone()))
        .collect::<FormValues>();
    values.insert("provider", "unused");
    values.insert("model", "fixture-model");
    let submit = Command::SubmitForm {
        name: "agents".into(),
        form: form.id.clone(),
        values: Box::new(values),
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(state.path().join("profiles.lock"))
        .unwrap();
    let lease = crate::storage::FileLease::exclusive(file).unwrap();
    assert!(
        profile_command(&b, submit.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("edited elsewhere")
    );
    drop(lease);
    remove(&mut a, "providers", "unused").await;
    assert!(
        profile_command(&b, submit)
            .await
            .unwrap_err()
            .to_string()
            .contains("Unknown provider")
    );
    assert!(!b._storage.database.with_file_name("agents.json").exists());
    a.close().await.unwrap();
    b.close().await.unwrap();
}

#[tokio::test]
async fn provider_deletion_refuses_a_project_file_of_an_unsupported_version() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let provider = saved_provider_id(&settings);
    remove(&mut app, "agents", "code").await;
    // Another project's file from before schema 4 cannot be checked for
    // references: the deletion is refused rather than guessed.
    let old = state.path().join("projects/old/agents.json");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, r#"{"schema":3,"selected":null,"agents":{}}"#).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let error = invoke(&app, "providers", &format!("delete {provider}"))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("projects/old/agents.json") && error.contains("unsupported version"),
        "{error}"
    );
    app.close().await.unwrap();
}
