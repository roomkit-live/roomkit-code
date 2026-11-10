//! A subscription with a pasted key, connected and edited from /providers.
use super::*;
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

async fn submit(app: &Application, form: FormSpec, changes: &[(&str, &str)]) {
    let mut values = form
        .fields
        .iter()
        .map(|field| (field.name.clone(), field.value.clone()))
        .collect::<FormValues>();
    for (name, value) in changes {
        values.insert(*name, *value);
    }
    let result = profile_command(
        app,
        Command::SubmitForm {
            name: form.command,
            form: form.id,
            values: Box::new(values),
        },
    )
    .await
    .unwrap();
    assert!(matches!(result, CommandResult::Switch(_)));
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
    submit(&app, form(&connect), &[("key", " go-test-key\n")]).await;
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
    submit(&app, form(&edit), &[("label", "My Go")]).await;
    let connection = saved(&settings, "opencode-go-1");
    assert_eq!(connection["label"], "My Go");
    assert_eq!(connection["authentication"]["key"], "go-test-key");

    // A second subscription takes the next ID.
    let second = providers(&app, "subscription:opencode-go").await;
    submit(&app, form(&second), &[("key", "other-key")]).await;
    assert_eq!(
        saved(&settings, "opencode-go-2")["authentication"]["key"],
        "other-key"
    );

    // No menu or form ever shows the key, and nothing reached a model.
    for events in [
        menu,
        connect,
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
