//! Prove the selected agent, engine, real tool, projection and UI as one path.
use super::*;
use roomkit_tui::render::{Renderers, screen_with_options};

fn assert_schemas(request: &Value) {
    let tools = request["tools"].as_array().unwrap();
    assert!(!tools.is_empty());
    for tool in tools {
        assert_eq!(
            tool["function"]["parameters"]["properties"]["_roomkit_intent"]["type"],
            "string"
        );
    }
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
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let session = turn(&app).await;
    let call = session
        .snapshot
        .as_ref()
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
    assert_schemas(&mock.requests.lock().unwrap()[0]);
    let frame = screen_with_options(
        &session,
        100,
        40,
        &Renderers::default(),
        &settings.ui,
        Duration::ZERO,
    );
    let text = frame.lines.join("\n");
    assert!(text.contains("Je vérifie le projet 文档"));
    assert!(!text.contains("printf fixture-result"));
    assert!(!text.contains("fixture-result"));
    // Editing the profile reconstructs its agent through the same factory.
    edit_profile(&mut app, "agents", "edit:code", &[("label", "Reviewer")]).await;
    turn_operation(&app, "after-profile-edit").await;
    assert_schemas(mock.requests.lock().unwrap().last().unwrap());
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
        &Renderers::default(),
        &settings.ui,
        Duration::ZERO,
    );
    assert!(frame.lines.join("\n").contains("Je vérifie le projet 文档"));
    app.close().await.unwrap();
}
