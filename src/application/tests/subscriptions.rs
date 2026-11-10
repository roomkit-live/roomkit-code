//! A subscription with a pasted key, connected and edited from /providers.
use super::*;
use crate::profiles::SUBSCRIPTION_SERVER;
use crate::profiles::model::{Authentication, Provider};
use roomkit_tui::protocol::{FormSpec, FormValues};
use roomkit_tui_engine::CommandResult;

const GO: &str = "https://opencode.ai/inference/go/openai/v1";

async fn providers(app: &Application, arguments: &str) -> Vec<Event> {
    let result = profile_command(
        app,
        Command::Invoke {
            name: "providers".into(),
            arguments: arguments.into(),
        },
    )
    .await
    .unwrap();
    let CommandResult::Events(events) = result else {
        panic!("Expected events")
    };
    events
}

fn form(events: &[Event]) -> FormSpec {
    events
        .iter()
        .find_map(|event| match event {
            Event::Form { form, .. } => Some(form.clone()),
            _ => None,
        })
        .unwrap()
}

async fn submit(app: &Application, form: FormSpec, changes: &[(&str, &str)]) -> CommandResult {
    let mut values = form
        .fields
        .iter()
        .map(|field| (field.name.clone(), field.value.clone()))
        .collect::<FormValues>();
    for (name, value) in changes {
        values.insert(*name, *value);
    }
    profile_command(
        app,
        Command::SubmitForm {
            name: form.command,
            form: form.id,
            values: Box::new(values),
        },
    )
    .await
    .unwrap()
}

/// The models a saved connection's answer offers, by their choices' values.
fn offered(result: &CommandResult) -> Vec<String> {
    let CommandResult::Events(events) = result else {
        panic!("Expected the connection's models")
    };
    let Event::Choices {
        command, choices, ..
    } = &events[0]
    else {
        panic!("Expected the connection's models, not {:?}", events[0])
    };
    assert_eq!(command, "providers");
    choices.iter().map(|c| c.value.clone()).collect()
}

/// A server that lists models as OpenCode Go's does, keeping the
/// authorization each listing carried.
struct Catalogue {
    url: String,
    authorizations: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Catalogue {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let authorizations = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&authorizations);
        let entry = |id: &str, api: &str, price: f64| {
            json!({"id": id, "object": "model", "api": {api: {}},
                "cost": {"text": {"input": {"price": price}}}, "limit": {"context": 262_144}})
        };
        let list = json!({"object": "list", "data": [
            entry("kimi-k3", "chat-completions", 0.6),
            entry("beam-free", "chat-completions", 0.0),
            entry("minimax-m3", "messages", 0.3),
        ]});
        let router = Router::new().route(
            "/models",
            axum::routing::get(move |headers: axum::http::HeaderMap| async move {
                let authorization = headers.get("authorization").map(|v| v.to_str().unwrap());
                seen.lock()
                    .unwrap()
                    .push(authorization.unwrap_or_default().into());
                Json(list)
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            url,
            authorizations,
            task,
        }
    }
}

impl Drop for Catalogue {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn saved(settings: &Settings, id: &str) -> Value {
    let file: Value = serde_json::from_slice(
        &std::fs::read(settings.state_dir.join("connections.json")).unwrap(),
    )
    .unwrap();
    file["profiles"][id].clone()
}

#[tokio::test]
async fn opencode_go_connects_with_a_pasted_key_and_edits_without_retyping_it() {
    let catalogue = Catalogue::start().await;
    SUBSCRIPTION_SERVER
        .scope(catalogue.url.clone(), connect_and_edit(&catalogue))
        .await;
}

async fn connect_and_edit(catalogue: &Catalogue) {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, "fixture-key".into())
        .await
        .unwrap();

    let menu = providers(&app, "").await;
    let Event::Choices { choices, .. } = &menu[0] else {
        panic!("Expected the providers menu")
    };
    assert!(
        choices
            .iter()
            .any(|c| c.value == "subscription:opencode-go")
    );

    // Its form asks for a name, a key and a timeout, never an address.
    let connect = providers(&app, "subscription:opencode-go").await;
    let fields: Vec<_> = form(&connect)
        .fields
        .iter()
        .map(|f| f.name.clone())
        .collect();
    assert_eq!(fields, ["label", "key", "timeout"]);
    // Saved, it offers the models it can call, listed with its key.
    let connected = submit(&app, form(&connect), &[("key", " go-test-key\n")]).await;
    assert_eq!(offered(&connected), ["model:opencode-go-1:kimi-k3"]);
    assert_eq!(
        *catalogue.authorizations.lock().unwrap(),
        ["Bearer go-test-key"]
    );
    let connection = saved(&settings, "opencode-go-1");
    assert_eq!(connection["subscription"], "opencode-go");
    assert_eq!(connection["base_url"], GO);
    assert_eq!(connection["kind"], "openai");
    assert_eq!(connection["label"], "OpenCode Go");
    assert_eq!(connection["authentication"]["key"], "go-test-key");

    // Editing opens the same form, the key masked and kept when left empty.
    let edit = providers(&app, "edit:opencode-go-1").await;
    let key = form(&edit)
        .fields
        .into_iter()
        .find(|f| f.name == "key")
        .unwrap();
    assert!(key.value.is_empty());
    let edited = submit(&app, form(&edit), &[("label", "My Go")]).await;
    assert!(matches!(edited, CommandResult::Switch(_)));
    let connection = saved(&settings, "opencode-go-1");
    assert_eq!(connection["label"], "My Go");
    assert_eq!(connection["authentication"]["key"], "go-test-key");

    // A second subscription takes the next ID.
    let second = providers(&app, "subscription:opencode-go").await;
    let connected_again = submit(&app, form(&second), &[("key", "other-key")]).await;
    assert_eq!(offered(&connected_again), ["model:opencode-go-2:kimi-k3"]);
    assert_eq!(
        saved(&settings, "opencode-go-2")["authentication"]["key"],
        "other-key"
    );

    // Choosing a model opens a new agent's form on that connection.
    let agent = form(&providers(&app, "model:opencode-go-1:kimi-k3").await);
    let value = |name: &str| {
        let field = agent.fields.iter().find(|f| f.name == name);
        field.unwrap().value.clone()
    };
    assert_eq!(
        (value("provider"), value("model")),
        ("opencode-go-1".into(), "kimi-k3".into())
    );

    // No menu or form ever shows the key, and nothing reached a model.
    let CommandResult::Events(connected) = connected else {
        unreachable!("offered the models")
    };
    for events in [
        menu,
        connect,
        connected,
        edit,
        providers(&app, "open:opencode-go-1").await,
    ] {
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains("go-test-key")
        );
    }
    assert!(mock.requests.lock().unwrap().is_empty());
    app.close().await.unwrap();
}

#[test]
fn a_subscription_connection_reaches_only_its_service_with_a_key() {
    let go = Provider {
        subscription: Some("opencode-go".into()),
        kind: crate::providers::ProviderKind::OpenAi,
        id: "opencode-go-1".into(),
        label: "OpenCode Go".into(),
        base_url: GO.into(),
        authentication: Authentication::Stored { key: "k".into() },
        timeout_seconds: 600,
    };
    assert!(go.validate().is_ok());
    assert!(
        Provider {
            authentication: Authentication::Environment {
                variable: "OPENCODE_API_KEY".into()
            },
            ..go.clone()
        }
        .validate()
        .is_ok()
    );
    let refused = [
        Provider {
            base_url: "https://other.example/v1".into(),
            ..go.clone()
        },
        Provider {
            base_url: "https://opencode.ai/inference/openai/v1".into(),
            ..go.clone()
        },
        Provider {
            kind: crate::providers::ProviderKind::Anthropic,
            ..go.clone()
        },
        Provider {
            authentication: Authentication::None,
            ..go.clone()
        },
        Provider {
            subscription: Some("unknown".into()),
            ..go.clone()
        },
        // An account subscription is no pasted-key subscription.
        Provider {
            subscription: Some("chatgpt".into()),
            base_url: roomkit_auth::CHATGPT_API_ROOT.into(),
            ..go.clone()
        },
        // An account credential names its own subscription.
        Provider {
            base_url: roomkit_auth::CHATGPT_API_ROOT.into(),
            authentication: Authentication::ChatGpt {
                account: uuid::Uuid::new_v4().to_string(),
            },
            ..go.clone()
        },
    ];
    for provider in refused {
        assert!(provider.validate().is_err(), "{}", provider.base_url);
    }
}
