//! Global agents are created once and used by every project; their MCP
//! servers and skills stay each project's choice.
use super::*;
use roomkit_tui_engine::CommandResult;

fn json_file(path: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// The agent IDs `/agents` offers, with their scope.
async fn listed(app: &Application) -> Vec<String> {
    let CommandResult::Events(events) = profile_command(
        app,
        Command::Invoke {
            name: "agents".into(),
            arguments: String::new(),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected the agent list");
    };
    let Some(Event::Choices { choices, .. }) = events.into_iter().next() else {
        panic!("Expected choices");
    };
    choices
        .into_iter()
        .filter(|choice| choice.value.starts_with("open:"))
        .map(|choice| choice.description)
        .collect()
}

fn install_skill(root: &std::path::Path) {
    let skill = root.join(".agents/skills/project-review");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: project-review\ndescription: Review project structure\n---\nReview it.\n",
    )
    .unwrap();
}

#[tokio::test]
async fn a_global_agent_opens_ready_in_a_new_project_with_its_own_choices() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    // The first project starts from a configured connection, with its own agent.
    let a_settings = settings(first.path(), state.path(), &mock.base_url);
    let mut a = Application::open(&a_settings, String::new()).await.unwrap();
    let provider = saved_provider_id(&a_settings);
    edit_profile(
        &mut a,
        "agents",
        "add",
        &[
            ("id", "reviewer"),
            ("label", "Reviewer"),
            ("provider", &provider),
            ("model", "fixture-model"),
            ("tools", "read"),
        ],
    )
    .await;
    assert_eq!(a.view.agent.as_str(), "reviewer");
    let global = json_file(&state.path().join("agents.json"));
    assert_eq!(global["selected"], "reviewer");
    assert_eq!(global["agents"]["reviewer"]["model"], "fixture-model");
    assert!(
        global["agents"].get("code").is_none(),
        "The bootstrapped agent stays in its project"
    );
    a.close().await.unwrap();
    drop(a);

    // A new project without a connection of its own opens with that agent, ready.
    install_skill(second.path());
    let mut b_settings = settings(second.path(), state.path(), &mock.base_url);
    b_settings.base_url.clear();
    b_settings.model.clear();
    let mut b = Application::open(&b_settings, String::new()).await.unwrap();
    assert_eq!(b.view.agent.as_str(), "reviewer");
    assert!(
        b.view.submission_error.is_none(),
        "{:?}",
        b.view.submission_error
    );
    let agents = listed(&b).await;
    assert_eq!(agents.len(), 1, "{agents:?}");
    assert!(agents[0].starts_with("reviewer · global"), "{agents:?}");
    turn_operation(&b, "global-agent-turn").await;
    assert_eq!(
        mock.requests.lock().unwrap().last().unwrap()["model"],
        "fixture-model"
    );

    // Enabling a skill there is that project's choice, not the agent's everywhere.
    b.profiles
        .skills(&b.kit, b.view.clone(), "enable project-review")
        .await
        .unwrap();
    let b_project = json_file(&b._storage.database.with_file_name("agents.json"));
    assert_eq!(
        b_project["extensions"]["reviewer"]["skills"],
        json!(["project-review"])
    );
    assert!(b_project["agents"].as_object().unwrap().is_empty());
    let global = json_file(&state.path().join("agents.json"));
    assert_eq!(global["agents"]["reviewer"]["skills"], json!([]));

    // Moving it to this project takes its choices along and removes it elsewhere.
    edit_profile(&mut b, "agents", "edit:reviewer", &[("scope", "project")]).await;
    let b_project = json_file(&b._storage.database.with_file_name("agents.json"));
    assert_eq!(
        b_project["agents"]["reviewer"]["skills"],
        json!(["project-review"])
    );
    assert!(b_project.get("extensions").is_none());
    let global = json_file(&state.path().join("agents.json"));
    assert!(global["agents"].as_object().unwrap().is_empty());
    assert!(global["selected"].is_null());
    b.close().await.unwrap();
    drop(b);

    // The first project keeps its own agent and no longer sees the moved one.
    // Its conversation, where the moved agent was selected, opens without an
    // agent, saying why, rather than with another.
    let a = Application::open(&a_settings, String::new()).await.unwrap();
    assert_eq!(a.view.agent.as_str(), "reviewer");
    let refused = a.view.submission_error.clone().unwrap();
    assert!(refused.contains("reviewer, no longer exists"), "{refused}");
    let agents = listed(&a).await;
    assert_eq!(agents.len(), 1, "{agents:?}");
    assert!(agents[0].starts_with("code · project"), "{agents:?}");
    a.close().await.unwrap();
}

#[tokio::test]
async fn deleting_a_global_agent_says_it_leaves_every_project() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let provider = saved_provider_id(&settings);
    edit_profile(
        &mut app,
        "agents",
        "add",
        &[
            ("id", "shared"),
            ("label", "Shared"),
            ("provider", &provider),
            ("model", "fixture-model"),
            ("tools", ""),
        ],
    )
    .await;
    let CommandResult::Events(events) = profile_command(
        &app,
        Command::Invoke {
            name: "agents".into(),
            arguments: "delete:shared".into(),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected a confirmation");
    };
    let Some(Event::Choices { choices, .. }) = events.into_iter().next() else {
        panic!("Expected choices");
    };
    let confirm = choices
        .iter()
        .find(|choice| choice.value.starts_with("confirm-delete:"))
        .unwrap();
    assert!(confirm.description.contains("leaves every project"));
    let CommandResult::Switch(view) = profile_command(
        &app,
        Command::Invoke {
            name: "agents".into(),
            arguments: confirm.value.clone(),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected a switch");
    };
    app.view = *view;
    assert_eq!(app.view.agent.as_str(), "code");
    let global = json_file(&state.path().join("agents.json"));
    assert!(global["agents"].as_object().unwrap().is_empty());
    app.close().await.unwrap();
}

/// Submit `/agents` form `action` with `changes`; the refusal's text on failure.
async fn submit_agent(
    app: &mut Application,
    action: &str,
    changes: &[(&str, &str)],
) -> std::result::Result<(), String> {
    use roomkit_tui::protocol::FormValues;
    let CommandResult::Events(events) = profile_command(
        app,
        Command::Invoke {
            name: "agents".into(),
            arguments: action.into(),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected a form")
    };
    let form = events
        .into_iter()
        .find_map(|event| match event {
            Event::Form { form, .. } => Some(form),
            _ => None,
        })
        .unwrap();
    let mut values = form
        .fields
        .iter()
        .map(|field| (field.name.clone(), field.value.clone()))
        .collect::<FormValues>();
    for (name, value) in changes {
        values.insert(*name, *value);
    }
    match profile_command(
        app,
        Command::SubmitForm {
            name: form.command,
            form: form.id,
            values: Box::new(values),
        },
    )
    .await
    {
        Ok(CommandResult::Switch(view)) => {
            app.view = *view;
            Ok(())
        }
        Ok(_) => panic!("Expected refreshed view"),
        Err(error) => Err(error.to_string()),
    }
}

/// The default ID the `/agents` creation form proposes.
async fn proposed_id(app: &Application) -> String {
    let CommandResult::Events(events) = profile_command(
        app,
        Command::Invoke {
            name: "agents".into(),
            arguments: "add".into(),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected a form")
    };
    events
        .into_iter()
        .find_map(|event| match event {
            Event::Form { form, .. } => form.fields.into_iter().find(|f| f.name == "id"),
            _ => None,
        })
        .unwrap()
        .value
}

/// Delete agent `id` through its confirmation, after `between` ran.
async fn delete_agent(
    app: &mut Application,
    id: &str,
    between: impl FnOnce(),
) -> std::result::Result<(), String> {
    let CommandResult::Events(events) = profile_command(
        app,
        Command::Invoke {
            name: "agents".into(),
            arguments: format!("delete:{id}"),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected a confirmation");
    };
    let Some(Event::Choices { choices, .. }) = events.into_iter().next() else {
        panic!("Expected choices");
    };
    let confirm = choices
        .into_iter()
        .find(|choice| choice.value.starts_with("confirm-delete:"))
        .unwrap();
    between();
    match profile_command(
        app,
        Command::Invoke {
            name: "agents".into(),
            arguments: confirm.value,
        },
    )
    .await
    {
        Ok(CommandResult::Switch(view)) => {
            app.view = *view;
            Ok(())
        }
        Ok(_) => panic!("Expected a switch"),
        Err(error) => Err(error.to_string()),
    }
}

fn shared_agent<'a>(id: &'a str, provider: &'a str) -> [(&'a str, &'a str); 5] {
    [
        ("id", id),
        ("label", id),
        ("provider", provider),
        ("model", "fixture-model"),
        ("tools", ""),
    ]
}

#[tokio::test]
async fn an_id_another_project_owns_is_never_shared() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    // Both projects start with their own `code`; the first also owns `agent-1`.
    let a_settings = settings(first.path(), state.path(), &mock.base_url);
    let mut a = Application::open(&a_settings, String::new()).await.unwrap();
    let provider = saved_provider_id(&a_settings);
    let mut own = shared_agent("agent-1", &provider).to_vec();
    own.push(("scope", "project"));
    submit_agent(&mut a, "add", &own).await.unwrap();
    a.close().await.unwrap();
    drop(a);
    let b_settings = settings(second.path(), state.path(), &mock.base_url);
    let mut b = Application::open(&b_settings, String::new()).await.unwrap();
    assert_eq!(b.view.agent.as_str(), "code");

    // Sharing the second project's `code` would replace the first one's.
    let refused = submit_agent(&mut b, "edit:code", &[("scope", "global")]).await;
    assert!(
        refused
            .unwrap_err()
            .contains("Another project has its own agent"),
        "The move is refused"
    );
    assert!(!state.path().join("agents.json").exists());
    // A new agent's default ID avoids the IDs other projects use.
    assert_eq!(proposed_id(&b).await, "agent-2");

    // An ID nobody else uses moves to every project.
    let mut solo = shared_agent("solo", &provider).to_vec();
    solo.push(("scope", "project"));
    submit_agent(&mut b, "add", &solo).await.unwrap();
    submit_agent(&mut b, "edit:solo", &[("scope", "global")])
        .await
        .unwrap();
    let global = json_file(&state.path().join("agents.json"));
    assert_eq!(global["agents"]["solo"]["model"], "fixture-model");
    let b_project = json_file(&b._storage.database.with_file_name("agents.json"));
    assert!(b_project["agents"].get("solo").is_none());
    b.close().await.unwrap();
    drop(b);
    let a = Application::open(&a_settings, String::new()).await.unwrap();
    let agents = listed(&a).await;
    assert!(
        agents
            .iter()
            .any(|agent| agent.starts_with("solo · global")),
        "{agents:?}"
    );
    a.close().await.unwrap();
}

#[tokio::test]
async fn a_project_whose_global_agent_was_deleted_opens_with_one_it_can_use() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let a_settings = settings(first.path(), state.path(), &mock.base_url);
    let mut a = Application::open(&a_settings, String::new()).await.unwrap();
    let provider = saved_provider_id(&a_settings);
    submit_agent(&mut a, "add", &shared_agent("alpha", &provider))
        .await
        .unwrap();
    submit_agent(&mut a, "add", &shared_agent("beta", &provider))
        .await
        .unwrap();
    a.close().await.unwrap();
    drop(a);

    // The second project opens with the global agent used last, then selects it.
    let mut b_settings = settings(second.path(), state.path(), &mock.base_url);
    b_settings.base_url.clear();
    b_settings.model.clear();
    let b = Application::open(&b_settings, String::new()).await.unwrap();
    assert_eq!(b.view.agent.as_str(), "beta");
    let CommandResult::Switch(_) = profile_command(
        &b,
        Command::Invoke {
            name: "agents".into(),
            arguments: "select:beta".into(),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected a switch");
    };
    b.close().await.unwrap();
    drop(b);

    // Deleted from the first project, it leaves the second one too. The
    // conversation it was selected in opens without an agent, saying why,
    // rather than with another.
    let mut a = Application::open(&a_settings, String::new()).await.unwrap();
    delete_agent(&mut a, "beta", || {}).await.unwrap();
    a.close().await.unwrap();
    drop(a);
    let b = Application::open(&b_settings, String::new()).await.unwrap();
    assert_eq!(b.view.agent.as_str(), "beta");
    let refused = b.view.submission_error.clone().unwrap();
    assert!(refused.contains("beta, no longer exists"), "{refused}");
    b.close().await.unwrap();
    drop(b);
    // A new conversation opens with an agent the project can use.
    b_settings.session = None;
    let b = Application::open(&b_settings, String::new()).await.unwrap();
    assert_eq!(b.view.agent.as_str(), "alpha");
    assert!(
        b.view.submission_error.is_none(),
        "{:?}",
        b.view.submission_error
    );
    b.close().await.unwrap();
}

#[tokio::test]
async fn a_new_global_agent_never_inherits_an_earlier_ones_choices() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let a_settings = settings(first.path(), state.path(), &mock.base_url);
    let mut a = Application::open(&a_settings, String::new()).await.unwrap();
    let provider = saved_provider_id(&a_settings);
    submit_agent(&mut a, "add", &shared_agent("shared", &provider))
        .await
        .unwrap();
    a.close().await.unwrap();
    drop(a);

    // The second project enables a skill for it.
    install_skill(second.path());
    let mut b_settings = settings(second.path(), state.path(), &mock.base_url);
    b_settings.base_url.clear();
    b_settings.model.clear();
    let b = Application::open(&b_settings, String::new()).await.unwrap();
    b.profiles
        .skills(&b.kit, b.view.clone(), "enable project-review")
        .await
        .unwrap();
    let b_file = b._storage.database.with_file_name("agents.json");
    assert!(json_file(&b_file)["extensions"].get("shared").is_some());
    b.close().await.unwrap();
    drop(b);

    // Deleted, then created again under the same ID: a new agent.
    let mut a = Application::open(&a_settings, String::new()).await.unwrap();
    delete_agent(&mut a, "shared", || {}).await.unwrap();
    submit_agent(&mut a, "add", &shared_agent("shared", &provider))
        .await
        .unwrap();
    a.close().await.unwrap();
    drop(a);
    assert!(
        json_file(&b_file).get("extensions").is_none(),
        "The earlier agent's skills are not carried over"
    );
}

#[tokio::test]
async fn a_change_to_one_copy_leaves_the_other_file_alone() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    install_skill(root.path());
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let provider = saved_provider_id(&settings);
    submit_agent(&mut app, "add", &shared_agent("shared", &provider))
        .await
        .unwrap();

    // A move interrupted between its two writes leaves a copy in each file.
    let global_file = state.path().join("agents.json");
    let project_file = app._storage.database.with_file_name("agents.json");
    let mut project = json_file(&project_file);
    project["agents"]["shared"] = json_file(&global_file)["agents"]["shared"].clone();
    std::fs::write(&project_file, serde_json::to_vec(&project).unwrap()).unwrap();
    let CommandResult::Switch(view) = profile_command(
        &app,
        Command::Invoke {
            name: "agents".into(),
            arguments: "select:shared".into(),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected a switch");
    };
    app.view = *view;

    // The project's copy changes; the global one other projects use stays.
    app.profiles
        .skills(&app.kit, app.view.clone(), "enable project-review")
        .await
        .unwrap();
    assert_eq!(
        json_file(&project_file)["agents"]["shared"]["skills"],
        json!(["project-review"])
    );
    assert!(json_file(&global_file)["agents"].get("shared").is_some());

    // Deleting the project's copy leaves the global agent to every project.
    delete_agent(&mut app, "shared", || {}).await.unwrap();
    assert!(json_file(&global_file)["agents"].get("shared").is_some());
    let agents = listed(&app).await;
    assert!(
        agents
            .iter()
            .any(|agent| agent.starts_with("shared · global")),
        "{agents:?}"
    );
    app.close().await.unwrap();
}

#[tokio::test]
async fn deleting_a_project_agent_ignores_a_global_change_meanwhile() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let provider = saved_provider_id(&settings);
    submit_agent(&mut app, "add", &shared_agent("shared", &provider))
        .await
        .unwrap();
    // Another project rewrites the global file while the confirmation is open.
    let global_file = state.path().join("agents.json");
    delete_agent(&mut app, "code", || {
        let global = json_file(&global_file);
        std::fs::write(&global_file, serde_json::to_vec(&global).unwrap()).unwrap();
    })
    .await
    .unwrap();
    let agents = listed(&app).await;
    assert_eq!(agents.len(), 1, "{agents:?}");
    assert!(agents[0].starts_with("shared · global"), "{agents:?}");
    app.close().await.unwrap();
}

#[tokio::test]
async fn a_stale_form_never_clears_a_live_agents_choices() {
    use roomkit_tui::protocol::FormValues;
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    install_skill(second.path());
    let a_settings = settings(first.path(), state.path(), &mock.base_url);
    let a = Application::open(&a_settings, String::new()).await.unwrap();
    let provider = saved_provider_id(&a_settings);
    let b_settings = settings(second.path(), state.path(), &mock.base_url);
    let mut b = Application::open(&b_settings, String::new()).await.unwrap();
    // The first project opens its creation form.
    let CommandResult::Events(events) = profile_command(
        &a,
        Command::Invoke {
            name: "agents".into(),
            arguments: "add".into(),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected a form")
    };
    let form = events
        .into_iter()
        .find_map(|event| match event {
            Event::Form { form, .. } => Some(form),
            _ => None,
        })
        .unwrap();

    // Meanwhile the second creates global `x` and enables a skill for it.
    submit_agent(&mut b, "add", &shared_agent("x", &provider))
        .await
        .unwrap();
    b.profiles
        .skills(&b.kit, b.view.clone(), "enable project-review")
        .await
        .unwrap();

    // The first form, opened before `x` existed, submits `x`: refused, and
    // the second project keeps its choices for the live agent.
    let mut values = form
        .fields
        .iter()
        .map(|field| (field.name.clone(), field.value.clone()))
        .collect::<FormValues>();
    for (name, value) in shared_agent("x", &provider) {
        values.insert(name, value);
    }
    let refused = profile_command(
        &a,
        Command::SubmitForm {
            name: form.command,
            form: form.id,
            values: Box::new(values),
        },
    )
    .await;
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("Profiles changed"),
        "The stale form is refused"
    );
    let b_file = b._storage.database.with_file_name("agents.json");
    assert_eq!(
        json_file(&b_file)["extensions"]["x"]["skills"],
        json!(["project-review"])
    );
    a.close().await.unwrap();
    b.close().await.unwrap();
}

#[tokio::test]
async fn deleting_a_global_agent_changed_meanwhile_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let provider = saved_provider_id(&settings);
    submit_agent(&mut app, "add", &shared_agent("shared", &provider))
        .await
        .unwrap();
    let global_file = state.path().join("agents.json");
    let refused = delete_agent(&mut app, "shared", || {
        let global = json_file(&global_file);
        std::fs::write(&global_file, serde_json::to_vec(&global).unwrap()).unwrap();
    })
    .await;
    assert!(refused.unwrap_err().contains("Profiles changed"));
    assert!(json_file(&global_file)["agents"].get("shared").is_some());
    app.close().await.unwrap();
}

#[tokio::test]
async fn an_unreadable_project_file_refuses_sharing_and_names_it() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let provider = saved_provider_id(&settings);
    // Another project's file is damaged: a conflict there cannot be ruled out.
    let other = state.path().join("projects/damaged");
    std::fs::create_dir_all(&other).unwrap();
    let damaged = other.join("agents.json");
    std::fs::write(&damaged, b"{ not json").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&damaged, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let refused = submit_agent(&mut app, "add", &shared_agent("shared", &provider)).await;
    let message = refused.unwrap_err();
    assert!(message.contains("Cannot read"), "{message}");
    assert!(
        message.contains("projects/damaged/agents.json"),
        "{message}"
    );
    assert!(!state.path().join("agents.json").exists());
    app.close().await.unwrap();
}
