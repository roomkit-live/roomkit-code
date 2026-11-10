//! Bounded connection checks; credentials stay on their selected protocol.
use super::model::Provider;
use crate::providers::ProviderKind;
use roomkit_ai::AiProvider;
use roomkit_anthropic::{AnthropicAiProvider, AnthropicConfig};
use std::time::Duration;

pub(super) async fn discover(provider: &Provider) -> Result<Vec<(String, Option<u64>)>, String> {
    provider.validate()?;
    if provider.kind == ProviderKind::OpenAi {
        return openai(provider).await;
    }
    let mut config = AnthropicConfig::new(provider.authentication.key()?, "discovery");
    config.base_url = Some(provider.base_url.clone());
    config.timeout = Duration::from_secs(10);
    let client =
        AnthropicAiProvider::new(config).map_err(|_| "Cannot initialize connection check")?;
    let models = tokio::time::timeout(Duration::from_secs(10), client.list_models())
        .await
        .map_err(|_| "Model discovery timed out")?
        .map_err(|_| "Anthropic model discovery failed; verify the URL, network and credential")?;
    accepted(
        models
            .into_iter()
            .map(|m| (m.id, m.context_window))
            .collect(),
    )
}

async fn openai(provider: &Provider) -> Result<Vec<(String, Option<u64>)>, String> {
    provider.validate()?;
    let client = crate::http_client::builder()
        .map_err(|_| "Cannot initialize connection check")?
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| "Cannot initialize connection check")?;
    let mut request = client.get(format!(
        "{}/models",
        provider.base_url.trim_end_matches('/')
    ));
    let key = provider.authentication.key()?;
    if !key.is_empty() {
        request = request.bearer_auth(key);
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| "Connection failed; verify the URL, network and credential")?;
    if !response.status().is_success() {
        return Err(format!(
            "Model discovery returned HTTP {}; verify the URL and authentication (some servers do not support /models)",
            response.status().as_u16()
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Could not read model catalogue")?
    {
        if body.len() + chunk.len() > 1_048_576 {
            return Err("Model catalogue exceeds 1 MiB".into());
        }
        body.extend_from_slice(&chunk);
    }
    let json: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| "Server did not return a JSON model catalogue")?;
    let models = json
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or("Server did not return a model list")?
        .iter()
        .filter_map(|model| {
            Some((
                model.get("id")?.as_str()?.to_owned(),
                model
                    .get("max_model_len")
                    .and_then(serde_json::Value::as_u64),
            ))
        })
        .filter(|(id, _)| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
        .take(500)
        .collect::<Vec<_>>();
    accepted(models)
}

fn accepted(models: Vec<(String, Option<u64>)>) -> Result<Vec<(String, Option<u64>)>, String> {
    let mut models: Vec<_> = models
        .into_iter()
        .filter(|(id, _)| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
        .take(500)
        .collect();
    models.sort();
    models.dedup_by(|a, b| a.0 == b.0);
    if models.is_empty() {
        return Err(
            "Connection succeeded but no models were returned; enter a model manually in /agents"
                .into(),
        );
    }
    Ok(models)
}
