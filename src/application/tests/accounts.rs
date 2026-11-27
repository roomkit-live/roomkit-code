//! Account menus are host behavior; OAuth wire tests live in roomkit-auth.
use super::*;
use crate::profiles::model::{Authentication, Provider};
use roomkit_tui_engine::CommandResult;

#[tokio::test]
async fn opening_account_menus_does_not_start_login_or_change_existing_connections() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, "fixture-key".to_owned())
        .await
        .unwrap();
    let before = std::fs::read(settings.state_dir.join("connections.json")).unwrap();
    for (arguments, expected) in [
        ("", "google:menu"),
        ("", "chatgpt:menu"),
        ("chatgpt:menu", "chatgpt:connect"),
    ] {
        let result = profile_command(
            &app,
            Command::Invoke {
                name: "providers".into(),
                arguments: arguments.into(),
            },
        )
        .await
        .unwrap();
        let CommandResult::Events(events) = result else {
            panic!("Expected a menu")
        };
        let Event::Choices { choices, .. } = &events[0] else {
            panic!("Expected account choices")
        };
        assert!(choices.iter().any(|c| c.value == expected));
        let serialized = serde_json::to_string(&events).unwrap();
        assert!(!serialized.contains("fixture-key"));
    }
    assert!(app.profiles.accounts.auth.accounts().unwrap().is_empty());
    assert!(!settings.state_dir.join("auth/accounts.json").exists());
    assert_eq!(
        std::fs::read(settings.state_dir.join("connections.json")).unwrap(),
        before
    );
    assert!(mock.requests.lock().unwrap().is_empty());
    let error = profile_command(
        &app,
        Command::Invoke {
            name: "providers".into(),
            arguments: format!("chatgpt-use:{}", uuid::Uuid::new_v4()),
        },
    )
    .await
    .err()
    .unwrap()
    .to_string();
    assert!(error.contains("Unknown ChatGPT account"));
    app.close().await.unwrap();
}

#[test]
fn oauth_accounts_cannot_forward_credentials_to_a_custom_endpoint_or_static_key_path() {
    let mut provider = Provider {
        subscription: None,
        kind: crate::providers::ProviderKind::OpenAi,
        id: "account-provider".into(),
        label: "ChatGPT".into(),
        base_url: roomkit_auth::CHATGPT_API_ROOT.into(),
        authentication: Authentication::ChatGpt {
            account: uuid::Uuid::new_v4().to_string(),
        },
        timeout_seconds: 600,
    };
    assert!(provider.validate().is_ok());
    provider.kind = crate::providers::ProviderKind::Anthropic;
    assert!(provider.validate().is_err());
    provider.kind = crate::providers::ProviderKind::OpenAi;
    assert!(provider.authentication.key().is_err());
    let saved = serde_json::to_value(&provider).unwrap();
    assert_eq!(saved["authentication"].as_object().unwrap().len(), 2);
    assert_eq!(saved["authentication"]["kind"], "chat_gpt");
    for url in [
        "http://api.openai.com/v1",
        "https://other.example/v1",
        "https://api.openai.com/v1/other",
        "https://api.openai.com.evil.example/v1",
    ] {
        provider.base_url = url.into();
        assert!(provider.validate().is_err());
    }
}

#[tokio::test]
async fn missing_account_credentials_fail_safely_without_anonymous_fallback() {
    use roomkit_providers_openai::TokenSource;
    let directory = tempfile::tempdir().unwrap();
    let auth = roomkit_auth::ChatGpt::new(directory.path().join("auth"), "Fixture App").unwrap();
    let source = crate::accounts::Credential::ChatGpt(auth, uuid::Uuid::new_v4().to_string());
    assert_eq!(
        source.token().await.unwrap_err(),
        roomkit_providers_openai::TokenError::SignInRequired
    );
}

#[tokio::test]
async fn stale_callback_forms_are_refused_without_saving_their_secret() {
    use roomkit_tui::protocol::FormValues;
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, "fixture-key".to_owned())
        .await
        .unwrap();
    let mut values = FormValues::new();
    values.insert(
        "callback",
        "http://127.0.0.1:23456/auth/callback?code=PRIVATE_CALLBACK&state=wrong",
    );
    let error = profile_command(
        &app,
        Command::SubmitForm {
            name: "providers".into(),
            form: "chatgpt-callback:stale".into(),
            values: Box::new(values),
        },
    )
    .await
    .err()
    .unwrap()
    .to_string();
    assert!(error.contains("sign-in form"));
    assert!(!error.contains("PRIVATE_CALLBACK"));
    assert!(app.profiles.accounts.auth.accounts().unwrap().is_empty());
    app.close().await.unwrap();
    let history = std::fs::read(&app._storage.database).unwrap();
    assert!(
        !history
            .windows(b"PRIVATE_CALLBACK".len())
            .any(|window| window == b"PRIVATE_CALLBACK")
    );
}

#[tokio::test]
async fn google_account_form_saves_only_the_reference_and_lists_models_with_its_token() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let settings = settings(root.path(), state.path(), "http://localhost");
    let dir = settings.state_dir.join("auth");
    let google =
        roomkit_auth::Google::new(&dir, "fixture-client", "fixture-installed-client").unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let saved = json!({"version":1,"host":"urn:uuid:test","accounts":{&id:{
        "id":id,"client_id":"fixture-client","issuer":"https://accounts.google.com","subject":"test",
        "email":"example@example.test","revision":"initial","tokens":{"access":"fixture-access","refresh":"fixture-refresh",
        "id":"fixture-id","expires_at":4_000_000_000_u64,"scopes":["https://www.googleapis.com/auth/cloud-platform"]}
    }}});
    crate::storage::write_atomic(
        &dir.join("google/accounts.json"),
        &serde_json::to_vec(&saved).unwrap(),
    )
    .unwrap();
    let router = Router::new().route("/v1beta/models", axum::routing::get(|headers: axum::http::HeaderMap| async move {
        assert_eq!(headers["authorization"],"Bearer fixture-access");
        assert_eq!(headers["x-goog-user-project"],"example-project");
        assert!(!headers.contains_key("x-goog-api-key"));
        Json(json!({"models":[{"name":"models/gemini-fixture","supportedGenerationMethods":["generateContent"]}]}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let app = crate::accounts::TEST_GOOGLE_CLIENT
        .scope(google, Application::open(&settings, String::new()))
        .await
        .unwrap();
    let action = format!("google-use:{id}");
    let submit = submit_profile(
        &app,
        "providers",
        &action,
        &[("project", "example-project")],
    );
    let result = crate::profiles::GEMINI_SERVER
        .scope(address, submit)
        .await
        .unwrap();
    let CommandResult::Events(events) = result else {
        panic!("Expected Gemini models")
    };
    let listed = serde_json::to_string(&events).unwrap();
    assert!(listed.contains("gemini-fixture"), "{listed}");
    let file = std::fs::read_to_string(settings.state_dir.join("connections.json")).unwrap();
    assert!(file.contains(&id) && file.contains("example-project"));
    for credential in [
        "fixture-access",
        "fixture-refresh",
        "fixture-installed-client",
    ] {
        assert!(!file.contains(credential) && !listed.contains(credential));
    }
    app.close().await.unwrap();
    server.abort();
}
