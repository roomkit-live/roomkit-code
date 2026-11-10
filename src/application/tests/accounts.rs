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
    let app = Application::open(&settings, "fixture-key".into())
        .await
        .unwrap();
    let before = std::fs::read(settings.state_dir.join("connections.json")).unwrap();
    for (arguments, expected) in [("", "chatgpt:menu"), ("chatgpt:menu", "chatgpt:connect")] {
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
    use roomkit_openai::TokenSource;
    let directory = tempfile::tempdir().unwrap();
    let auth = roomkit_auth::ChatGpt::new(directory.path().join("auth"), "Fixture App").unwrap();
    let source = crate::accounts::Credential {
        auth,
        account: uuid::Uuid::new_v4().to_string(),
    };
    assert_eq!(
        source.token().await.unwrap_err(),
        roomkit_openai::TokenError::SignInRequired
    );
}
