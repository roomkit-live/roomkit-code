//! A conversation keeps the agent selected in it.
use super::*;
use crate::sessions::designated;
use roomkit::orchestration::{ConversationState, get_conversation_state, save_conversation_state};
use roomkit::{ConversationStore, model::RoomId};
use roomkit_tui_engine::{CommandHandler, CommandResult};

/// Runs the session command `name` from the application's view, and shows
/// the view it switches to.
async fn switch(app: &mut Application, settings: &Settings, name: &str, arguments: &str) {
    let command = Command::Invoke {
        name: name.into(),
        arguments: arguments.into(),
    };
    let operation = format!("{name}-{arguments}");
    let room = app.view.room.as_str();
    let request = Request::new(RequestId::new(7), operation.as_str(), room, command);
    let router = app.router(&settings.workspace);
    let Ok(CommandResult::Switch(view)) = router.handle(request, app.view.clone()).await else {
        panic!("Expected /{name} to switch the view")
    };
    app.view = *view;
}

/// A conversation where `code` answered, and a second one, shown, where the
/// person added the agent `reviewer`, on another provider, which answered.
async fn two_conversations(
    app: &mut Application,
    settings: &Settings,
    other: &Mock,
) -> [RoomId; 2] {
    let one = app.view.room.clone();
    turn_operation(app, "first-turn").await;
    switch(app, settings, "new", "").await;
    let two = app.view.room.clone();
    assert_ne!(one, two);
    let connection = [
        ("id", "secondary"),
        ("label", "Second connection"),
        ("url", other.base_url.as_str()),
        ("auth", "stored"),
        ("key", "fixture-private-key"),
    ];
    add_connection(app, &connection).await;
    let reviewer = [
        ("id", "reviewer"),
        ("label", "Reviewer"),
        ("provider", "secondary"),
        ("model", "second-model"),
    ];
    edit_profile(app, "agents", "add", &reviewer).await;
    assert_eq!(app.view.room, two);
    assert_eq!(app.view.agent.as_str(), "reviewer");
    turn_operation(app, "second-agent-turn").await;
    [one, two]
}

/// The state conversation `room` stores.
async fn state_of(app: &Application, room: &RoomId) -> ConversationState {
    let stored = app.kit.store().get_room(room).await.unwrap().unwrap();
    get_conversation_state(&stored).unwrap()
}

/// Found in use: an agent selected in another conversation followed the
/// view back, and the next message sent this conversation's history to it,
/// and to its provider.
#[tokio::test]
async fn a_conversation_keeps_the_agent_selected_in_it() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let first = Mock::with_answer(vec![], "First agent answer").await;
    let second = Mock::with_answer(vec![], "Second agent answer").await;
    let mut settings = settings(root.path(), state.path(), &first.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let [one, two] = two_conversations(&mut app, &settings, &second).await;
    // The engine's own record of who handles each conversation.
    assert_eq!(
        state_of(&app, &one).await.active_agent_id.unwrap().as_str(),
        "code"
    );
    let changed = state_of(&app, &two).await;
    assert_eq!(changed.active_agent_id.unwrap().as_str(), "reviewer");
    assert_eq!(changed.previous_agent_id.unwrap().as_str(), "code");
    assert_eq!(changed.handoff_count, 1);

    // Back in the first one: its own agent, and nothing of it reaches the
    // provider of the agent selected elsewhere.
    switch(&mut app, &settings, "resume", one.as_str()).await;
    assert_eq!(app.view.agent.as_str(), "code");
    assert_eq!(app.view.submission_error, None);
    turn_operation(&app, "back-in-the-first").await;
    assert_eq!(first.requests.lock().unwrap().len(), 2);
    assert_eq!(second.requests.lock().unwrap().len(), 1);
    switch(&mut app, &settings, "resume", two.as_str()).await;
    assert_eq!(app.view.agent.as_str(), "reviewer");
    app.close().await.unwrap();
    drop(app);

    // The project's last selection is the reviewer. At the next launch, the
    // first conversation still opens with its own agent.
    settings.base_url.clear();
    settings.model.clear();
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(app.view.room, one);
    assert_eq!(app.view.agent.as_str(), "code");
    app.close().await.unwrap();
    drop(app);
    // A new conversation opens with the project's selection, and keeps it.
    settings.session = None;
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(app.view.agent.as_str(), "reviewer");
    let draft = app.view.room.clone();
    let opened = state_of(&app, &draft).await;
    assert_eq!(opened.active_agent_id.unwrap().as_str(), "reviewer");

    // The person selects another agent in it and sends nothing, then selects
    // the reviewer again elsewhere: the project's selection.
    select(&mut app, "code").await;
    let chosen = state_of(&app, &draft).await;
    assert_eq!(chosen.active_agent_id.unwrap().as_str(), "code");
    switch(&mut app, &settings, "resume", two.as_str()).await;
    select(&mut app, "reviewer").await;
    app.close().await.unwrap();
    drop(app);
    // The next launch reuses that draft, where nothing was sent: it takes
    // the agent it is opened with.
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(app.view.room, draft);
    assert_eq!(app.view.agent.as_str(), "reviewer");
    let reused = state_of(&app, &draft).await;
    assert_eq!(reused.active_agent_id.unwrap().as_str(), "reviewer");
    app.close().await.unwrap();
}

/// Selects agent `id` in the conversation shown.
async fn select(app: &mut Application, id: &str) {
    let select = Command::Invoke {
        name: "agents".into(),
        arguments: format!("select:{id}"),
    };
    let Ok(CommandResult::Switch(view)) = profile_command(app, select).await else {
        panic!("Expected the selection to switch the view")
    };
    app.view = *view;
}

/// Deletes agent `id` through `/agents`, as the person confirms it.
async fn delete(app: &mut Application, id: &str) {
    let ask = Command::Invoke {
        name: "agents".into(),
        arguments: format!("delete {id}"),
    };
    let Ok(CommandResult::Events(events)) = profile_command(app, ask).await else {
        panic!("Expected a confirmation")
    };
    let Event::Choices { choices, .. } = &events[0] else {
        panic!("Expected choices")
    };
    let confirm = Command::Invoke {
        name: "agents".into(),
        arguments: choices[1].value.clone(),
    };
    let Ok(CommandResult::Switch(view)) = profile_command(app, confirm).await else {
        panic!("Expected the deletion to refresh the view")
    };
    app.view = *view;
}

/// A conversation stored before its agent was recorded designates the agent
/// that worked in it last. One whose agent was deleted opens without an
/// agent, saying why, never with another.
#[tokio::test]
async fn an_unrecorded_agent_is_the_last_at_work_and_a_deleted_one_is_not_replaced() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let first = Mock::with_answer(vec![], "First agent answer").await;
    let second = Mock::with_answer(vec![], "Second agent answer").await;
    let mut settings = settings(root.path(), state.path(), &first.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let [one, two] = two_conversations(&mut app, &settings, &second).await;
    // As two conversations stored before: neither records an agent.
    for room in [&one, &two] {
        let blank = ConversationState::new();
        save_conversation_state(app.kit.store(), room, &blank)
            .await
            .unwrap();
    }
    assert_eq!(designated(&app.kit, &one).await.unwrap().as_str(), "code");
    assert_eq!(
        designated(&app.kit, &two).await.unwrap().as_str(),
        "reviewer"
    );
    switch(&mut app, &settings, "resume", one.as_str()).await;
    assert_eq!(app.view.agent.as_str(), "code");

    // The reviewer is deleted from the first conversation, which keeps its
    // agent. The second one still designates the reviewer.
    delete(&mut app, "reviewer").await;
    assert_eq!(app.view.agent.as_str(), "code");
    switch(&mut app, &settings, "resume", two.as_str()).await;
    assert_eq!(app.view.agent.as_str(), "reviewer");
    let refused = app.view.submission_error.clone().unwrap();
    assert!(refused.contains("reviewer, no longer exists"), "{refused}");
    app.close().await.unwrap();
    drop(app);

    // The same at the next launch, on that conversation.
    settings.base_url.clear();
    settings.model.clear();
    settings.session = Some(two.to_string());
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(app.view.agent.as_str(), "reviewer");
    let refused = app.view.submission_error.clone().unwrap();
    assert!(refused.contains("reviewer, no longer exists"), "{refused}");
    // Until the person selects an agent there.
    select(&mut app, "code").await;
    assert_eq!(app.view.submission_error, None);
    assert_eq!(designated(&app.kit, &two).await.unwrap().as_str(), "code");
    turn_operation(&app, "the-agent-selected-there").await;
    assert_eq!(first.requests.lock().unwrap().len(), 2);
    assert_eq!(second.requests.lock().unwrap().len(), 1);
    app.close().await.unwrap();
}
