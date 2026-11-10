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

fn values(choices: &[Choice]) -> Vec<&str> {
    choices.iter().map(|c| c.value.as_str()).collect()
}

async fn invoke(app: &Application, command: &str, arguments: &str) -> CommandResult {
    profile_command(
        app,
        Command::Invoke {
            name: command.into(),
            arguments: arguments.into(),
        },
    )
    .await
    .unwrap()
}

/// The form a command's answer opens, by its fields' values.
fn opened(result: &CommandResult) -> std::collections::BTreeMap<String, String> {
    let CommandResult::Events(events) = result else {
        panic!("Expected a form")
    };
    let Some(Event::Form { form, .. }) = events.last() else {
        panic!("Expected a form, not {events:?}")
    };
    form.fields
        .iter()
        .map(|f| (f.name.clone(), f.value.clone()))
        .collect()
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
    assert_eq!(
        values(choices),
        ["model:second:discovered-model", "typed:second"]
    );
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
    let [Event::Notice { text, .. }, Event::Choices { choices, .. }] = events.as_slice() else {
        panic!("Expected a notice and a way to type the model, not {events:?}")
    };
    assert!(
        text.starts_with("Connection saved. Model discovery returned HTTP 404"),
        "{text}"
    );
    assert_eq!(values(choices), ["typed:third"]);
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

#[tokio::test]
async fn model_on_a_connection_listing_none_says_how_to_set_one() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let unlisted = format!("{}/unlisted", mock.base_url);
    let settings = settings(root.path(), state.path(), &unlisted);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let refused = profile_command(
        &app,
        Command::Invoke {
            name: "model".into(),
            arguments: String::new(),
        },
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        refused.starts_with("Model discovery returned HTTP 404")
            && refused.ends_with("; /model <ID> sets a model by its ID"),
        "{refused}"
    );
    let CommandResult::Switch(view) = model(&app, "typed-model").await else {
        panic!("Expected refreshed view")
    };
    app.view = *view;
    assert_eq!(app.view.model.as_ref().unwrap().model, "typed-model");
    app.close().await.unwrap();
}

/// A server whose model list waits until its test lets it answer, telling
/// when a listing reached it.
struct Held {
    url: String,
    arrived: Arc<tokio::sync::Notify>,
    gate: Arc<tokio::sync::Mutex<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Held {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let arrived = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let (reached, open) = (Arc::clone(&arrived), Arc::clone(&gate));
        let router = Router::new().route(
            "/v1/models",
            axum::routing::get(move || async move {
                reached.notify_one();
                let _open = open.lock().await;
                Json(json!({"data": [{"id": "held-model"}]}))
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            url,
            arrived,
            gate,
            task,
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// What `listing` answers, `/effort <effort>`, which takes the edit lease
/// and the profiles, having been saved while `server` held that listing.
async fn answered_while_held(
    app: &Application,
    server: &Held,
    listing: impl std::future::Future<Output = CommandResult>,
    effort: &str,
) -> CommandResult {
    let held = Arc::clone(&server.gate).lock_owned().await;
    let meanwhile = async {
        server.arrived.notified().await;
        let invoke = Command::Invoke {
            name: "effort".into(),
            arguments: effort.into(),
        };
        let changed = tokio::time::timeout(Duration::from_secs(5), profile_command(app, invoke));
        let changed = changed.await;
        drop(held);
        changed
    };
    let (answer, changed) = tokio::join!(listing, meanwhile);
    let Ok(changed) = changed else {
        panic!("/effort waited on a listing")
    };
    let changed = changed.unwrap();
    assert!(matches!(changed, CommandResult::Switch(_)));
    answer
}

#[tokio::test]
async fn a_listing_waiting_on_its_server_holds_up_no_other_profile_command() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let server = Held::start().await;
    let settings = settings(root.path(), state.path(), &server.url);
    let app = Application::open(&settings, String::new()).await.unwrap();

    let listed = answered_while_held(&app, &server, model(&app, ""), "high").await;
    assert_eq!(offered(&listed).1[0].value, "held-model");

    let connecting = async {
        let changes = [("id", "second"), ("url", server.url.as_str())];
        submit_profile(&app, "providers", "add", &changes)
            .await
            .unwrap()
    };
    let connected = answered_while_held(&app, &server, connecting, "low").await;
    assert_eq!(offered(&connected).1[0].value, "model:second:held-model");

    let testing = async {
        let invoke = Command::Invoke {
            name: "providers".into(),
            arguments: "test:second".into(),
        };
        profile_command(&app, invoke).await.unwrap()
    };
    let tested = answered_while_held(&app, &server, testing, "default").await;
    assert_eq!(offered(&tested).1[0].value, "model:second:held-model");

    // A new agent's models, then an existing agent's, in /agents.
    let creating = invoke(&app, "agents", "new:second");
    let creation = answered_while_held(&app, &server, creating, "high").await;
    assert_eq!(offered(&creation).1[0].value, "model:second:held-model");
    let changing = invoke(&app, "agents", "models:code");
    let change = answered_while_held(&app, &server, changing, "low").await;
    assert_eq!(offered(&change).1[0].value, "use-model:code:held-model");
    app.close().await.unwrap();
}

#[tokio::test]
async fn adding_an_agent_lists_its_connections_models_before_its_form() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();

    // One connection, the one the launch imported: + Add agent lists its
    // models at once.
    let file: Value = serde_json::from_slice(
        &std::fs::read(settings.state_dir.join("connections.json")).unwrap(),
    )
    .unwrap();
    let local = file["profiles"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let menu = invoke(&app, "agents", "").await;
    assert_eq!(offered(&menu).1[0].value, "new");
    let listed = invoke(&app, "agents", "new").await;
    let (command, choices) = offered(&listed);
    assert_eq!(command, "providers");
    assert_eq!(
        values(choices),
        [
            format!("model:{local}:discovered-model"),
            format!("typed:{local}")
        ]
    );
    let form = opened(
        &invoke(
            &app,
            "providers",
            &format!("model:{local}:discovered-model"),
        )
        .await,
    );
    assert_eq!(
        (&form["provider"][..], &form["model"][..]),
        (local.as_str(), "discovered-model")
    );
    let form = opened(&invoke(&app, "providers", &format!("typed:{local}")).await);
    assert_eq!(
        (&form["provider"][..], &form["model"][..]),
        (local.as_str(), "")
    );

    // Several: it asks for the connection first; one without a list leaves
    // the model to be typed.
    let unlisted = format!("{}/unlisted", mock.base_url);
    add_connection(&app, &[("id", "second"), ("url", &mock.base_url)]).await;
    submit_profile(
        &app,
        "providers",
        "add",
        &[("id", "third"), ("url", &unlisted)],
    )
    .await
    .unwrap();
    let connections = invoke(&app, "agents", "new").await;
    let (command, choices) = offered(&connections);
    assert_eq!(command, "agents");
    assert_eq!(
        values(choices),
        [
            format!("new:{local}"),
            "new:second".into(),
            "new:third".into()
        ]
    );
    let listed = invoke(&app, "agents", "new:second").await;
    assert_eq!(
        values(offered(&listed).1),
        ["model:second:discovered-model", "typed:second"]
    );
    let CommandResult::Events(events) = invoke(&app, "agents", "new:third").await else {
        panic!("Expected a notice")
    };
    let [Event::Notice { text, .. }, Event::Choices { choices, .. }] = events.as_slice() else {
        panic!("Expected a notice and a way to type the model, not {events:?}")
    };
    assert!(
        text.starts_with("Model discovery returned HTTP 404"),
        "{text}"
    );
    assert_eq!(values(choices), ["typed:third"]);

    // Saving the form preset from the list creates and selects the agent.
    edit_profile(
        &mut app,
        "providers",
        "model:second:discovered-model",
        &[("id", "listed"), ("tools", "")],
    )
    .await;
    assert_eq!(app.view.agent.as_str(), "listed");
    assert_eq!(app.view.model.as_ref().unwrap().model, "discovered-model");
    app.close().await.unwrap();
}

#[tokio::test]
async fn an_agent_changes_its_model_from_its_connections_list_in_its_form() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();

    let menu = invoke(&app, "agents", "open:code").await;
    assert!(values(offered(&menu).1).contains(&"models:code"));
    let listed = invoke(&app, "agents", "models:code").await;
    let (command, choices) = offered(&listed);
    assert_eq!(command, "agents");
    assert_eq!(values(choices), ["use-model:code:discovered-model"]);

    // The agent's own form opens with that model, its other settings kept.
    let mut form = opened(&invoke(&app, "agents", "use-model:code:discovered-model").await);
    let mut edited = opened(&invoke(&app, "agents", "edit:code").await);
    assert_eq!(
        (&form["id"][..], &form["model"][..]),
        ("code", "discovered-model")
    );
    form.remove("model");
    edited.remove("model");
    assert_eq!(form, edited);
    edit_profile(&mut app, "agents", "use-model:code:discovered-model", &[]).await;
    assert_eq!(app.view.model.as_ref().unwrap().model, "discovered-model");
    turn_operation(&app, "after-change").await;
    assert_eq!(
        mock.requests.lock().unwrap().last().unwrap()["model"],
        "discovered-model"
    );
    let listed = invoke(&app, "agents", "models:code").await;
    assert_eq!(offered(&listed).1[0].description, "Current model");
    app.close().await.unwrap();
}
