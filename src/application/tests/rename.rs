//! `/rename` names a conversation; its ID stays its identity.
use super::*;
use roomkit::ConversationStore;
use roomkit_tui_engine::{CommandHandler, CommandResult};

async fn invoke(
    app: &Application,
    settings: &Settings,
    view: &SessionConfig,
    name: &str,
    arguments: &str,
) -> std::result::Result<CommandResult, String> {
    let request = Request::new(
        RequestId::new(1),
        format!("{name}-{arguments}").as_str(),
        view.room.as_str(),
        Command::Invoke {
            name: name.into(),
            arguments: arguments.into(),
        },
    );
    app.router(&settings.workspace)
        .handle(request, view.clone())
        .await
        .map_err(|rejection| rejection.message)
}

#[tokio::test]
async fn a_named_conversation_keeps_its_name_in_the_title_the_picker_and_the_exit_report() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let mut settings = settings(project.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let room = app.view.room.clone();
    // An empty conversation is the next draft: nothing to name yet.
    let refused = invoke(&app, &settings, &app.view, "rename", "Release notes").await;
    assert!(refused.unwrap_err().contains("first message"));
    turn(&app).await;
    for invalid in ["", "two\nlines"] {
        assert!(
            invoke(&app, &settings, &app.view, "rename", invalid)
                .await
                .is_err()
        );
    }
    let CommandResult::Events(events) =
        invoke(&app, &settings, &app.view, "rename", "  Release notes ")
            .await
            .unwrap()
    else {
        panic!("Expected the title change");
    };
    // The view renames the conversation in place, without switching.
    let [
        Event::TitleChanged {
            conversation,
            title,
        },
    ] = events.as_slice()
    else {
        panic!("Expected one title change: {events:?}");
    };
    assert_eq!(conversation.as_str(), room.as_str());
    // The name leads, so a narrow header cuts the path, not the name.
    assert!(title.starts_with("Release notes · "), "{title}");
    // It renames while a response runs too: no switch is involved.
    assert!(app.router(&settings.workspace).allow_while_busy("rename"));
    // A stored name this version would refuse reads as no name.
    let key = "roomkit_code:name";
    let invalid = [("name".into(), json!("a\u{202E}b"))].into_iter().collect();
    app.kit
        .store()
        .set_private_state(&room, key, Some(invalid))
        .await
        .unwrap();
    assert_eq!(crate::sessions::name(&app.kit, &room).await, None);
    let valid = [("name".into(), json!("Release notes"))]
        .into_iter()
        .collect();
    app.kit
        .store()
        .set_private_state(&room, key, Some(valid))
        .await
        .unwrap();
    let summary = crate::exit_summary::collect(
        &app.kit,
        &app.profiles.resources.processes,
        Some((room.to_string(), None)),
    )
    .await;
    let lines = summary.lines(true);
    assert_eq!(lines[0], format!("Session: Release notes · {room}"));
    app.close().await.unwrap();
    drop(app);

    // Reopened by its ID, it keeps its name.
    settings.session = Some(room.to_string());
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert!(
        app.view.title.starts_with("Release notes · "),
        "{}",
        app.view.title
    );
    app.close().await.unwrap();
    drop(app);

    // Another session's picker lists it by its name, under its ID.
    settings.session = None;
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_ne!(app.view.room, room);
    let CommandResult::Events(events) = invoke(&app, &settings, &app.view, "resume", "")
        .await
        .unwrap()
    else {
        panic!("Expected the session catalogue");
    };
    let Event::Choices { choices, .. } = &events[0] else {
        panic!("Expected choices");
    };
    let named = choices
        .iter()
        .find(|choice| choice.value == room.as_str())
        .unwrap();
    assert_eq!(named.label, "Release notes");
    // Its first message stays where the picker's filter finds it.
    assert!(
        named.description.starts_with("Inspect the project. · "),
        "{}",
        named.description
    );
    app.close().await.unwrap();
}

#[tokio::test]
async fn a_conversation_is_renamed_while_a_response_runs() {
    use roomkit_tui::protocol::Decision;
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![("write", json!({"path":"held.txt","content":"x"}))]).await;
    let settings = settings(project.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(
        app.session()
            .run_with_commands(host, app.router(&settings.workspace)),
    );
    assert!(matches!(event(&client).await, Event::Snapshot(_)));
    command(
        &client,
        1,
        "main",
        Command::Submit {
            agent: "code".into(),
            text: "Write the file".into(),
        },
    );
    // The turn waits on this approval: the conversation is executing.
    let held = approval(&client).await;
    command(
        &client,
        2,
        "main",
        Command::Invoke {
            name: "rename".into(),
            arguments: "Held turn".into(),
        },
    );
    let title = loop {
        match event(&client).await {
            Event::Reply { request, error, .. } if request == RequestId::new(2) => {
                assert!(error.is_none(), "{error:?}");
            }
            Event::TitleChanged { title, .. } => break title,
            _ => {}
        }
    };
    assert!(title.starts_with("Held turn · "), "{title}");
    command(
        &client,
        3,
        "main",
        Command::Decide {
            permission: held.id.clone(),
            decision: Decision::Deny,
        },
    );
    loop {
        if let Event::RunFinished { .. } = event(&client).await {
            break;
        }
    }
    let room = app.view.room.clone();
    assert_eq!(
        crate::sessions::name(&app.kit, &room).await.as_deref(),
        Some("Held turn")
    );
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}
