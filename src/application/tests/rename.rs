//! `/rename` names a conversation; its ID stays its identity.
use super::*;
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
    assert!(view.title.ends_with(" · Release notes"), "{}", view.title);
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
        app.view.title.ends_with(" · Release notes"),
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
    app.close().await.unwrap();
}
