use super::*;
use roomkit_cli_roomkit::CommandResult;

fn install(root: &std::path::Path, body: &str) {
    let skill = root.join(".agents/skills/project-review");
    std::fs::create_dir_all(skill.join("references")).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        format!("---\nname: project-review\ndescription: Review project structure\n---\n{body}\n"),
    )
    .unwrap();
    std::fs::write(
        skill.join("references/checklist.md"),
        "REFERENCE-CHECKLIST: inspect public entry points.",
    )
    .unwrap();
}

async fn command(app: &Application, arguments: &str) -> std::result::Result<CommandResult, String> {
    app.profiles
        .skills(&app.kit, app.view.clone(), arguments)
        .await
}

fn system(request: &Value) -> String {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "system")
        .map(|m| m["content"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn skills_are_opt_in_progressive_and_resume_for_the_same_agent() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    install(
        root.path(),
        "PRIVATE-SKILL-BODY: read references/checklist.md before your review.",
    );
    let mock = Mock::start(vec![
        ("activate_skill", json!({"name":"project-review"})),
        (
            "read_skill_reference",
            json!({"skill_name":"project-review","filename":"checklist.md"}),
        ),
    ])
    .await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    assert!(
        !app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name == "activate_skill")
    );
    assert!(command(&app, "use project-review").await.is_err());
    let listing = format!("{:?}", command(&app, "list").await.unwrap());
    assert!(listing.contains("project-review · disabled"));
    assert!(!listing.contains("PRIVATE-SKILL-BODY"));
    command(&app, "enable project-review").await.unwrap();
    let prepared = command(
        &app,
        "use project-review Inspect https://example.test/api:version",
    )
    .await
    .unwrap();
    assert!(
        matches!(prepared, CommandResult::Submit(text) if text.contains("Inspect https://example.test/api:version"))
    );
    assert!(
        app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name == "activate_skill")
    );
    let session = turn_operation(&app, "skills-first").await;
    assert!(session.snapshot.as_ref().unwrap().items.iter().any(|item| matches!(item, Item::Tool { call: t, .. } if t.name == "activate_skill" && t.state == ToolState::Succeeded)));
    let requests = mock.requests.lock().unwrap().clone();
    assert!(system(&requests[0]).contains("project-review"));
    assert!(!system(&requests[0]).contains("PRIVATE-SKILL-BODY"));
    assert!(requests[1].to_string().contains("PRIVATE-SKILL-BODY"));
    assert!(requests[2].to_string().contains("REFERENCE-CHECKLIST"));
    // Editing unrelated agent settings preserves its skill selection.
    edit_profile(&mut app, "agents", "edit:code", &[("label", "Reviewer")]).await;
    app.close().await.unwrap();
    drop(app);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    turn_operation(&app, "skills-resumed").await;
    assert!(system(mock.requests.lock().unwrap().last().unwrap()).contains("PRIVATE-SKILL-BODY"));
    // A different agent in the same room does not inherit this catalogue.
    let provider = saved_provider_id(&settings);
    edit_profile(
        &mut app,
        "agents",
        "add",
        &[
            ("id", "observer"),
            ("provider", &provider),
            ("model", "fixture-model"),
        ],
    )
    .await;
    turn_operation(&app, "observer").await;
    let request = mock.requests.lock().unwrap().last().unwrap().clone();
    assert!(!system(&request).contains("PRIVATE-SKILL-BODY"));
    assert!(
        !request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "activate_skill")
    );
    app.close().await.unwrap();
}

#[tokio::test]
async fn reload_updates_instructions_and_missing_or_disabled_skills_leave_the_catalogue() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    install(root.path(), "VERSION-ONE");
    let mock = Mock::start(vec![("activate_skill", json!({"name":"project-review"}))]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    command(&app, "enable project-review").await.unwrap();
    turn_operation(&app, "activate").await;
    install(root.path(), "VERSION-TWO");
    command(&app, "reload").await.unwrap();
    turn_operation(&app, "updated").await;
    let prompt = system(mock.requests.lock().unwrap().last().unwrap());
    assert!(prompt.contains("VERSION-TWO") && !prompt.contains("VERSION-ONE"));
    command(&app, "disable project-review").await.unwrap();
    turn_operation(&app, "disabled").await;
    assert!(!system(mock.requests.lock().unwrap().last().unwrap()).contains("VERSION-TWO"));
    command(&app, "enable project-review").await.unwrap();
    std::fs::remove_file(root.path().join(".agents/skills/project-review/SKILL.md")).unwrap();
    command(&app, "reload").await.unwrap();
    assert!(command(&app, "use project-review").await.is_err());
    assert!(
        !app.profiles
            .tools("code")
            .await
            .iter()
            .any(|t| t.name == "activate_skill")
    );
    app.close().await.unwrap();
}
