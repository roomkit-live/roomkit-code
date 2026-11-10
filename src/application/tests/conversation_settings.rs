//! A conversation keeps the model, the effort and the mode set in it.
use super::*;
use crate::permissions::Mode;
use roomkit_tui_engine::{CommandHandler, CommandResult};

/// Runs the application command `name` from the application's view.
async fn run(app: &Application, settings: &Settings, name: &str, arguments: &str) -> CommandResult {
    let command = Command::Invoke {
        name: name.into(),
        arguments: arguments.into(),
    };
    let operation = format!("{name}-{arguments}");
    let room = app.view.room.as_str();
    let request = Request::new(RequestId::new(7), operation.as_str(), room, command);
    let router = app.router(&settings.workspace);
    let Ok(result) = router.handle(request, app.view.clone()).await else {
        panic!("/{name} {arguments} was refused")
    };
    result
}

/// Runs a command that changes the view, and shows the view it gives.
async fn switch(app: &mut Application, settings: &Settings, name: &str, arguments: &str) {
    let CommandResult::Switch(view) = run(app, settings, name, arguments).await else {
        panic!("Expected /{name} to give a view")
    };
    app.view = *view;
}

/// The model and the effort the view shows its agent running with.
fn runs_with(app: &Application) -> (String, String) {
    let shown = app.view.model.as_ref().unwrap();
    (shown.model.clone(), shown.effort.clone().unwrap())
}

/// The latest request the server received.
fn asked(mock: &Mock) -> Value {
    mock.requests.lock().unwrap().last().unwrap().clone()
}

/// Found in use: a model chosen in another conversation changed the agent
/// itself, and this conversation's next message would have gone to it.
#[tokio::test]
async fn a_conversation_keeps_the_model_and_the_effort_set_in_it() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mock = Mock::start(vec![]).await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let one = app.view.room.clone();
    turn_operation(&app, "in-the-first").await;
    switch(&mut app, &settings, "new", "").await;
    let two = app.view.room.clone();
    assert_ne!(one, two);

    // Another model and another effort, in the second conversation.
    switch(&mut app, &settings, "model", "other-model").await;
    switch(&mut app, &settings, "effort", "high").await;
    assert_eq!(runs_with(&app), ("other-model".into(), "high".into()));
    turn_operation(&app, "in-the-second").await;
    let request = asked(&mock);
    assert_eq!(request["model"], "other-model");
    assert_eq!(request["reasoning_effort"], "high");
    // The agent's own, which a new conversation starts with, are as they were.
    let agents = app._storage.database.with_file_name("agents.json");
    let own: Value = serde_json::from_slice(&std::fs::read(agents).unwrap()).unwrap();
    assert_eq!(own["agents"]["code"]["model"], "fixture-model");
    assert!(own["agents"]["code"]["reasoning_effort"].is_null());

    // Back in the first, in the same launch: the model it started with.
    switch(&mut app, &settings, "resume", one.as_str()).await;
    assert_eq!(runs_with(&app), ("fixture-model".into(), "default".into()));
    turn_operation(&app, "back-in-the-first").await;
    let request = asked(&mock);
    assert_eq!(request["model"], "fixture-model");
    assert!(request.get("reasoning_effort").is_none());
    // And the second one still runs with its own.
    switch(&mut app, &settings, "resume", two.as_str()).await;
    turn_operation(&app, "back-in-the-second").await;
    assert_eq!(asked(&mock)["model"], "other-model");
    app.close().await.unwrap();
    drop(app);

    // At the next launch, each one opens as it was left.
    settings.base_url.clear();
    settings.model.clear();
    for (room, model, effort) in [
        (&one, "fixture-model", "default"),
        (&two, "other-model", "high"),
    ] {
        settings.session = Some(room.to_string());
        let app = Application::open(&settings, String::new()).await.unwrap();
        assert_eq!(runs_with(&app), (model.into(), effort.into()));
        turn_operation(&app, "after-the-restart").await;
        assert_eq!(asked(&mock)["model"], model);
        app.close().await.unwrap();
    }
    // A new conversation starts with the agent's own.
    settings.session = None;
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let draft = app.view.room.clone();
    assert_eq!(runs_with(&app), ("fixture-model".into(), "default".into()));
    // A model is set in it and nothing is sent: the next launch opens that
    // draft again, which starts anew.
    switch(&mut app, &settings, "model", "other-model").await;
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(app.view.room, draft);
    assert_eq!(runs_with(&app), ("fixture-model".into(), "default".into()));
    app.close().await.unwrap();
}

/// The agent's own model, changed in its form from another conversation, or
/// changed before conversations kept their settings, leaves a conversation
/// on the model it ran with.
#[tokio::test]
async fn a_model_set_for_the_agent_elsewhere_leaves_a_conversation_as_it_was() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mock = Mock::start(vec![]).await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let one = app.view.room.clone();
    turn_operation(&app, "in-the-first").await;
    switch(&mut app, &settings, "new", "").await;

    // The agent's form, saved from the second conversation with another
    // model: that conversation takes it, as the conversations to come do.
    edit_profile(&mut app, "agents", "edit:code", &[("model", "form-model")]).await;
    assert_eq!(runs_with(&app).0, "form-model");
    turn_operation(&app, "in-the-second").await;
    assert_eq!(asked(&mock)["model"], "form-model");
    // The first one keeps the model it ran with.
    switch(&mut app, &settings, "resume", one.as_str()).await;
    assert_eq!(runs_with(&app).0, "fixture-model");
    turn_operation(&app, "back-in-the-first").await;
    assert_eq!(asked(&mock)["model"], "fixture-model");

    // As a conversation stored before conversations kept their settings: it
    // keeps nothing, and its agent's own model changed since.
    crate::sessions::forget(&app.kit, &one).await.unwrap();
    app.close().await.unwrap();
    drop(app);
    settings.base_url.clear();
    settings.model.clear();
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(app.view.room, one);
    // It opens on the model of its last answer, and keeps it from then on.
    assert_eq!(runs_with(&app).0, "fixture-model");
    turn_operation(&app, "after-the-restart").await;
    assert_eq!(asked(&mock)["model"], "fixture-model");
    let kept = crate::sessions::kept(&app.kit, &one).await;
    assert_eq!(kept.agents["code"].model, "fixture-model");
    app.close().await.unwrap();
}

/// The mode the view shows.
fn mode_of(app: &Application) -> String {
    app.view.mode.as_ref().unwrap().selected.clone()
}

/// A conversation opens in the mode it was left in, unless the launch names
/// one. A launch nobody watches keeps its own.
#[tokio::test]
async fn a_conversation_opens_in_the_mode_it_was_left_in() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mock = Mock::start(vec![]).await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let one = app.view.room.clone();
    turn_operation(&app, "in-the-first").await;
    let CommandResult::Events(_) = run(&app, &settings, "mode", "yolo").await else {
        panic!("Expected the mode to change")
    };
    assert_eq!(app.policy.mode(one.as_str()), Mode::Yolo);
    // A new conversation starts in the launch's mode.
    switch(&mut app, &settings, "new", "").await;
    let two = app.view.room.clone();
    assert_ne!(one, two);
    assert_eq!(mode_of(&app), "ask");
    app.close().await.unwrap();
    drop(app);

    // At the next launch, each one opens in the mode it was left in.
    settings.base_url.clear();
    settings.model.clear();
    for (room, mode) in [(&one, "yolo"), (&two, "ask")] {
        settings.session = Some(room.to_string());
        let app = Application::open(&settings, String::new()).await.unwrap();
        assert_eq!(mode_of(&app), mode);
        app.close().await.unwrap();
    }
    // A launch that names its mode opens in it, and the conversation keeps it.
    settings.session = Some(one.to_string());
    (settings.mode, settings.mode_given) = (Mode::Plan, true);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(mode_of(&app), "plan");
    app.close().await.unwrap();
    drop(app);
    (settings.mode, settings.mode_given) = (Mode::Ask, false);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(mode_of(&app), "plan");
    app.close().await.unwrap();
    drop(app);
    // A launch nobody watches keeps its own mode, and leaves the
    // conversation's.
    settings.unattended = true;
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(app.policy.mode(one.as_str()), Mode::Ask);
    app.close().await.unwrap();
    drop(app);
    settings.unattended = false;
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(mode_of(&app), "plan");
    app.close().await.unwrap();
}
