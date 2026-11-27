#![allow(clippy::unwrap_used, reason = "Invalid fixtures fail the test")]
use super::*;

fn connection(authentication: Authentication) -> Provider {
    Provider {
        subscription: None,
        kind: crate::providers::ProviderKind::OpenAi,
        id: "local".into(),
        label: "Local".into(),
        base_url: "http://localhost:8000/v1".into(),
        authentication,
        timeout_seconds: 600,
    }
}

fn configured(model: &str) -> ConfiguredPrice {
    let verified = roomkit_ai::Date::new(2026, 11, 6).unwrap();
    ConfiguredPrice {
        kind: crate::providers::ProviderKind::OpenAi,
        base_url: "http://localhost:8000/v1".into(),
        model: model.into(),
        pricing: roomkit_ai::ModelPricing::new(0.5, 1.5, verified).unwrap(),
    }
}

/// RMK-1461: the kit's catalogue prices its models, a ChatGPT account's
/// too, and the configuration a server's own.
#[test]
fn a_model_is_priced_by_the_catalogue_else_the_configuration() {
    let catalogue =
        <roomkit_providers_openai::OpenAi as roomkit_ai::AiProvider>::available_models()
            .into_iter()
            .find_map(|entry| Some((entry.id, entry.pricing?)))
            .unwrap();
    let key = connection(Authentication::None);
    let listed = list_price(&key, &catalogue.0, Some(&configured(&catalogue.0)));
    assert_eq!(listed, Some(catalogue.1.clone()));
    let local = configured("local-model");
    assert_eq!(
        list_price(&key, "local-model", Some(&local)),
        Some(local.pricing.clone())
    );
    assert_eq!(list_price(&key, "another-model", Some(&local)), None);
    let mut elsewhere = connection(Authentication::None);
    elsewhere.base_url = "http://another:8000/v1".into();
    assert_eq!(list_price(&elsewhere, "local-model", Some(&local)), None);
    let plan = connection(Authentication::ChatGpt {
        account: "account".into(),
    });
    assert_eq!(list_price(&plan, &catalogue.0, None), Some(catalogue.1));
}

#[test]
fn hosted_provider_profiles_require_their_own_api_credentials() {
    use crate::providers::ProviderKind;
    for kind in [ProviderKind::Gemini, ProviderKind::OpenRouter] {
        let mut profile = Provider {
            kind,
            base_url: kind.default_url().into(),
            ..connection(Authentication::None)
        };
        assert!(profile.validate().is_err());
        profile.authentication = Authentication::Environment {
            variable: "FIXTURE_KEY".into(),
        };
        assert!(profile.validate().is_ok());
        profile.authentication = Authentication::ChatGpt {
            account: uuid::Uuid::new_v4().to_string(),
        };
        assert!(profile.validate().is_err());
    }
}

#[test]
fn google_accounts_require_a_gemini_endpoint_and_an_explicit_quota_project() {
    use crate::providers::ProviderKind;
    let id = uuid::Uuid::new_v4().to_string();
    let kind = ProviderKind::Gemini;
    let auth = |project: &str| Authentication::Google {
        account: id.clone(),
        project: project.into(),
    };
    let mut provider = Provider {
        kind,
        base_url: kind.default_url().into(),
        ..connection(auth("example-project"))
    };
    assert!(provider.validate().is_ok());
    assert!(provider.authentication.key().is_err());
    for project in ["", "-example", "example-", "EXAMPLE", "a/b", "bad\r\nvalue"] {
        provider.authentication = auth(project);
        assert!(provider.validate().is_err());
    }
    provider.authentication = auth("example-project");
    provider.base_url = "https://another.example".into();
    assert!(provider.validate().is_err());
    provider.kind = ProviderKind::OpenAi;
    assert!(provider.validate().is_err());
}
