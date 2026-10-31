use super::*;
use roomkit_tui_engine::{CommandHandler, CommandResult};

fn handler(app: &Application, settings: &Settings) -> crate::sessions::Sessions {
    crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: settings.workspace.clone(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
        services: app.services.clone(),
    }
}

fn command(app: &Application, name: &str, arguments: &str) -> Request {
    Request::new(
        RequestId::new(1),
        format!("{name}-{arguments}").as_str(),
        app.view.room.as_str(),
        Command::Invoke {
            name: name.into(),
            arguments: arguments.into(),
        },
    )
}

#[tokio::test]
async fn independent_instances_share_a_project_without_sharing_a_conversation() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let mut settings = settings(project.path(), state.path(), &mock.base_url);
    settings.session = None;
    let (first, second) = tokio::join!(
        Application::open(&settings, String::new()),
        Application::open(&settings, String::new()),
    );
    let (first, second) = (first.unwrap(), second.unwrap());
    assert_ne!(
        first.view.room, second.view.room,
        "an occupied empty draft was reused"
    );
    let (one, two) = tokio::join!(
        turn_operation(&first, "one"),
        turn_operation(&second, "two")
    );
    assert_eq!(
        one.snapshot().unwrap().conversation.as_str(),
        first.view.room.as_str()
    );
    assert_eq!(
        two.snapshot().unwrap().conversation.as_str(),
        second.view.room.as_str()
    );
    let command_handler = handler(&second, &settings);
    let error = command_handler
        .handle(
            command(&second, "resume", first.view.room.as_str()),
            second.view.clone(),
        )
        .await
        .err()
        .unwrap();
    assert!(error.message.contains("open in another"), "{error}");
    let CommandResult::Events(events) = command_handler
        .handle(command(&second, "resume", ""), second.view.clone())
        .await
        .unwrap()
    else {
        panic!("Expected session picker");
    };
    assert!(
        matches!(&events[0], Event::Choices { choices, .. } if choices.iter().any(|c|
        c.value == first.view.room.as_str() && c.description.contains("Open in another instance")))
    );
    let first_messages = first.session().snapshot().await.unwrap().items;
    let second_messages = second.session().snapshot().await.unwrap().items;
    assert_eq!(first_messages.len(), second_messages.len());
    assert_eq!(mock.requests.lock().unwrap().len(), 2);
    // Failure must not affect the current conversation or reserve a missing one.
    assert!(
        command_handler
            .handle(command(&second, "resume", "missing"), second.view.clone())
            .await
            .is_err()
    );
    assert!(first.services.sessions.claim("missing").unwrap().is_some());
    first.close().await.unwrap();
    let CommandResult::Switch(next) = command_handler
        .handle(
            command(&second, "resume", first.view.room.as_str()),
            second.view.clone(),
        )
        .await
        .unwrap()
    else {
        panic!("Expected released conversation to resume");
    };
    assert_eq!(next.room, first.view.room);
    assert!(
        first
            .services
            .sessions
            .claim(first.view.room.as_str())
            .is_err(),
        "closed owners must stay closed"
    );
    second.close().await.unwrap();
}

#[tokio::test]
async fn named_session_has_one_owner_and_retains_history_after_release() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(project.path(), state.path(), &mock.base_url);
    let first = Application::open(&settings, String::new()).await.unwrap();
    let error = Application::open(&settings, String::new())
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("open in another"), "{error}");
    turn(&first).await;
    let before = first.session().snapshot().await.unwrap().items;
    // A view change leaves its engine's session hooks and processes open.
    let CommandResult::Switch(next) = handler(&first, &settings)
        .handle(command(&first, "new", ""), first.view.clone())
        .await
        .unwrap()
    else {
        panic!("Expected new conversation");
    };
    assert_ne!(next.room, first.view.room);
    assert!(Application::open(&settings, String::new()).await.is_err());
    first.close().await.unwrap();
    let resumed = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(resumed.session().snapshot().await.unwrap().items, before);
    resumed.close().await.unwrap();
}
