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
    let CommandResult::Switch(view) =
        invoke(&app, &settings, &app.view, "rename", "  Release notes ")
            .await
            .unwrap()
    else {
        panic!("Expected the renamed view");
    };
    assert_eq!(view.room, room);
    // The name leads, so a narrow header cuts the path, not the name.
    assert!(view.title.starts_with("Release notes · "), "{}", view.title);
    // The engine refuses commands while a response runs, before this
    // handler: a rename then never stores a name it cannot show.
    assert!(!app.router(&settings.workspace).allow_while_busy("rename"));
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
