//! Prove the selected agent, engine, real tool, projection and UI as one path.
use super::*;
use roomkit_tui::render::screen_with_options;

fn assert_schemas(request: &Value, required: bool) {
    // Keep the prompt's guidance alongside the schema's display field.
    // Check the instructions on the real request, including rebuilt agents.
    let instructions = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "system")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(instructions.contains(
        "For every tool call, include a short description of your next action in _roomkit_intent"
    ));
    assert!(instructions.contains("not in a separate message"));
    assert!(instructions.contains("respect any tool-owned parameter of the same name"));
    assert!(
        instructions.find("For every tool call, include").unwrap()
            > instructions.find("Apply tool-specific guidance").unwrap()
    );
    let tools = request["tools"].as_array().unwrap();
    assert!(!tools.is_empty());
    for tool in tools {
        assert_eq!(
            tool["function"]["parameters"]["properties"]["_roomkit_intent"]["type"],
            "string"
        );
        let contains_intent = tool["function"]["parameters"]["required"]
            .as_array()
            .is_some_and(|fields| fields.iter().any(|field| field == "_roomkit_intent"));
        assert_eq!(contains_intent, required);
    }
}

#[tokio::test]
async fn missing_public_intent_keeps_the_call_executable_and_displays_its_name() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("listed-fixture.txt"), "fixture").unwrap();
    let mock = Mock::start(vec![("list", json!({"path":"."}))]).await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.require_tool_intent = false;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let session = turn(&app).await;
    let text = screen_with_options(
        &session,
        100,
        40,
        &crate::tool_views::renderers(root.path()),
        &settings.ui,
        Duration::ZERO,
    )
    .lines
    .join("\n");
    assert!(text.contains("✓ list · F2 details"), "{text}");
    assert!(!text.contains("Action completed"));
    assert!(text.contains("listed-fixture.txt"));
    assert!(!text.contains("\"data\":"));
    assert_schemas(&mock.requests.lock().unwrap()[0], false);
    app.close().await.unwrap();
}

#[tokio::test]
async fn public_intents_reach_the_view_and_survive_profile_changes_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![(
        "shell",
        json!({
            "command": "printf fixture-result",
            "_roomkit_intent": "  Je vérifie\n le projet 文档\u{202e}\u{0000} "
        }),
    )])
    .await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.mode = crate::permissions::Mode::Yolo;
    // Include stdout after the shell's command, exit status and archive metadata.
    settings.ui.tool_preview_lines = 6;
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let session = turn(&app).await;
    let call = session
        .snapshot()
        .unwrap()
        .items
        .iter()
        .find_map(|item| match item {
            Item::Tool { call, .. } if call.name == "shell" => Some(call),
            _ => None,
        })
        .unwrap();
    assert_eq!(call.state, ToolState::Succeeded);
    assert_eq!(call.intent.as_deref(), Some("Je vérifie le projet 文档"));
    assert_eq!(call.arguments, json!({"command":"printf fixture-result"}));
    assert!(call.text.contains("fixture-result"));
    assert_schemas(&mock.requests.lock().unwrap()[0], true);
    let frame = screen_with_options(
        &session,
        100,
        40,
        &crate::tool_views::renderers(root.path()),
        &settings.ui,
        Duration::ZERO,
    );
    let text = frame.lines.join("\n");
    assert!(text.contains("Je vérifie le projet 文档"));
    assert!(text.contains("$ printf fixture-result"));
    assert!(text.contains("exit 0"));
    assert!(text.contains("│ fixture-result"), "{text}");
    assert!(!text.contains("\"data\":"));
    // Editing the profile reconstructs its agent through the same factory.
    edit_profile(&mut app, "agents", "edit:code", &[("label", "Reviewer")]).await;
    turn_operation(&app, "after-profile-edit").await;
    assert_schemas(mock.requests.lock().unwrap().last().unwrap(), true);
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let snapshot = app.session().snapshot().await.unwrap();
    assert!(snapshot.items.iter().any(|item| matches!(item, Item::Tool { call, .. }
        if call.intent.as_deref() == Some("Je vérifie le projet 文档") && call.state == ToolState::Succeeded)));
    let mut resumed = Session::default();
    resumed.apply(Event::Snapshot(snapshot));
    let frame = screen_with_options(
        &resumed,
        100,
        40,
        &crate::tool_views::renderers(root.path()),
        &settings.ui,
        Duration::ZERO,
    );
    assert!(frame.lines.join("\n").contains("Je vérifie le projet 文档"));
    turn_operation(&app, "after-restart").await;
    assert_schemas(mock.requests.lock().unwrap().last().unwrap(), true);
    app.close().await.unwrap();
}
