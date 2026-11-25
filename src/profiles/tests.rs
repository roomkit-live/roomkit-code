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
