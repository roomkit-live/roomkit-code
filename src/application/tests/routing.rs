//! The command router sends each command to its owner and refuses the rest
//! with a stable reason.
use super::*;
use roomkit_tui::protocol::FormValues;
use roomkit_tui_engine::CommandHandler;

async fn refusal(
    app: &Application,
    router: &crate::sessions::Sessions,
    command: Command,
) -> String {
    let request = Request::new(
        RequestId::new(1),
        "routing",
        app.view.room.as_str(),
        command,
    );
    match router.handle(request, app.view.clone()).await {
        Err(rejection) => rejection.message,
        Ok(_) => panic!("Expected a refusal"),
    }
}

fn invoke(name: &str, arguments: &str) -> Command {
    Command::Invoke {
        name: name.into(),
        arguments: arguments.into(),
    }
}

fn form(name: &str) -> Command {
    Command::SubmitForm {
        name: name.into(),
        form: "form".into(),
        values: Box::new(FormValues::default()),
    }
}

#[tokio::test]
async fn the_router_refuses_what_no_command_owns() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let router = app.router(&settings.workspace);
    // Only MCP and profile forms are submitted; other commands are invoked.
    for name in ["shell", "skills", "tools", "new"] {
        assert_eq!(
            refusal(&app, &router, form(name)).await,
            "Unsupported application request",
            "{name}"
        );
    }
    let interrupt = Command::Interrupt { run: "run".into() };
    assert_eq!(
        refusal(&app, &router, interrupt).await,
        "Unsupported application request"
    );
    assert_eq!(
        refusal(&app, &router, invoke("nonsense", "")).await,
        "Unknown application command"
    );
    assert_eq!(
        refusal(&app, &router, invoke("new", "now")).await,
        "Use /new without arguments"
    );
    assert_eq!(
        refusal(&app, &router, invoke("resume", "../other")).await,
        "Invalid session id; use /resume to choose from the list"
    );
    assert_eq!(
        refusal(&app, &router, invoke("tools", "missing")).await,
        "Unknown tool; use /tools to inspect registered capabilities"
    );
    app.close().await.unwrap();
}

/// `/model` is advertised to the terminal and reaches the profiles, which
/// list the current agent's models.
#[tokio::test]
async fn model_is_advertised_and_answered_by_the_profiles() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let router = app.router(&settings.workspace);
    assert!(
        crate::sessions::commands()
            .iter()
            .any(|c| c.name == "model")
    );
    let request = Request::new(
        RequestId::new(1),
        "routing",
        app.view.room.as_str(),
        invoke("model", ""),
    );
    let Ok(roomkit_tui_engine::CommandResult::Events(events)) =
        router.handle(request, app.view.clone()).await
    else {
        panic!("Expected the current agent's models")
    };
    assert!(
        matches!(&events[..], [Event::Choices { command, .. }] if command == "model"),
        "{events:?}"
    );
    app.close().await.unwrap();
}
