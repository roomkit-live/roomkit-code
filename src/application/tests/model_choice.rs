//! Choosing a model among those a connection serves: after adding the
//! connection, and for the current agent with `/model`.
use super::*;
use roomkit_tui::protocol::Choice;
use roomkit_tui_engine::CommandResult;

async fn model(app: &Application, arguments: &str) -> CommandResult {
    profile_command(
        app,
        Command::Invoke {
            name: "model".into(),
            arguments: arguments.into(),
        },
    )
    .await
    .unwrap()
}

/// The choices of a command's answer, with the command they invoke.
fn offered(result: &CommandResult) -> (&str, &[Choice]) {
    let CommandResult::Events(events) = result else {
        panic!("Expected choices")
    };
    let Event::Choices {
        command, choices, ..
    } = &events[0]
    else {
        panic!("Expected choices, not {:?}", events[0])
    };
    (command, choices)
}

#[tokio::test]
async fn a_new_connection_offers_its_models_and_one_listing_none_stays_saved() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();

    let connected = submit_profile(
        &app,
        "providers",
        "add",
        &[("id", "second"), ("url", &mock.base_url)],
    )
    .await
    .unwrap();
    let (command, choices) = offered(&connected);
    assert_eq!(command, "providers");
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].value, "model:second:discovered-model");
    assert_eq!(
        choices[0].description,
        "Reported capacity: 32768 tokens · create agent"
    );

    // A server without a model list keeps its connection, and says why.
    let unlisted = format!("{}/unlisted", mock.base_url);
    let CommandResult::Events(events) = submit_profile(
        &app,
        "providers",
        "add",
        &[("id", "third"), ("url", &unlisted)],
    )
    .await
    .unwrap() else {
        panic!("Expected a notice")
    };
    let [Event::Notice { text, .. }] = events.as_slice() else {
        panic!("Expected a notice, not {events:?}")
    };
    assert!(
        text.starts_with("Connection saved. Model discovery returned HTTP 404"),
        "{text}"
    );
    let file: Value = serde_json::from_slice(
        &std::fs::read(settings.state_dir.join("connections.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(file["profiles"]["third"]["base_url"], unlisted.as_str());
    assert!(mock.requests.lock().unwrap().is_empty());
    app.close().await.unwrap();
}

#[tokio::test]
async fn model_lists_the_current_connections_models_and_switches_the_agent() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let room = app.view.room.clone();

    let listed = model(&app, "").await;
    let (command, choices) = offered(&listed);
    assert_eq!(command, "model");
    assert_eq!(choices[0].value, "discovered-model");
    assert_eq!(choices[0].description, "Reported capacity: 32768 tokens");

    // A listed model, then one the server serves unlisted, colons included.
    for chosen in ["discovered-model", "llama3:8b"] {
        let CommandResult::Switch(view) = model(&app, chosen).await else {
            panic!("Expected refreshed view")
        };
        app.view = *view;
        assert_eq!(app.view.room, room);
        assert_eq!(app.view.model.as_ref().unwrap().model, chosen);
        turn_operation(&app, chosen).await;
        assert_eq!(
            mock.requests.lock().unwrap().last().unwrap()["model"],
            chosen
        );
    }
    let CommandResult::Switch(view) = model(&app, "discovered-model").await else {
        panic!("Expected refreshed view")
    };
    app.view = *view;
    let listed = model(&app, "").await;
    assert_eq!(offered(&listed).1[0].description, "Current model");

    // The choice lasts: a restart keeps the agent on it.
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(app.view.model.as_ref().unwrap().model, "discovered-model");
    app.close().await.unwrap();
}
