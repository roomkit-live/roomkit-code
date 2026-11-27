//! Construct a provider without mixing its wire settings with agent assembly.
use super::*;
use crate::providers::{CodeProvider, ProviderKind};
use roomkit_providers_anthropic::{Anthropic as AnthropicAiProvider, AnthropicConfig};
use roomkit_providers_gemini::{Gemini, GeminiConfig};
use roomkit_providers_openai::{OpenAi as OpenAiAiProvider, OpenRouter, OpenRouterConfig};
use std::time::Duration;

impl Profiles {
    /// The same connection and model this agent runs with in the named conversation.
    pub(crate) async fn title_provider(
        &self,
        room: &str,
        id: &str,
    ) -> Result<(CodeProvider, u32), String> {
        let state = self.state.lock().await;
        let mut profile = state.running_agent(room, id).ok_or("Unknown title agent")?;
        let connection = state
            .providers
            .value
            .profiles
            .get(&profile.provider)
            .ok_or("Unknown title connection")?;
        profile.reasoning_effort = None;
        const TITLE_TOKEN_LIMIT: u32 = 512;
        let max_tokens = profile
            .max_output_tokens
            .unwrap_or(TITLE_TOKEN_LIMIT)
            .min(TITLE_TOKEN_LIMIT);
        Ok((self.generation_provider(&profile, connection)?, max_tokens))
    }

    pub(super) fn generation_provider(
        &self,
        profile: &Agent,
        provider: &Provider,
    ) -> Result<CodeProvider, String> {
        match provider.kind {
            ProviderKind::LlamaCpp => self
                .local_models
                .provider(profile, provider, &self.workspace, self.diagnostics.clone())
                .map(CodeProvider::Local),
            ProviderKind::OpenAi => OpenAiAiProvider::new(self.openai_config(profile, provider)?)
                .map(CodeProvider::OpenAi)
                .map_err(|_| "Cannot initialize the OpenAI connection".into()),
            ProviderKind::Gemini => provider
                .gemini(&profile.model, profile.max_output_tokens)
                .map(|p| CodeProvider::Gemini {
                    provider: p,
                    vision: profile.vision,
                }),
            ProviderKind::OpenRouter => provider
                .openrouter(
                    &profile.model,
                    profile.max_output_tokens,
                    profile.reasoning_effort.clone(),
                )
                .map(|p| CodeProvider::OpenRouter {
                    provider: p,
                    vision: profile.vision,
                }),
            ProviderKind::Anthropic => self.anthropic_provider(profile, provider),
        }
    }

    fn anthropic_provider(
        &self,
        profile: &Agent,
        provider: &Provider,
    ) -> Result<CodeProvider, String> {
        let mut config = AnthropicConfig::new(provider.authentication.key()?, &profile.model);
        config.base_url = Some(provider.base_url.clone());
        config.timeout = std::time::Duration::from_secs(provider.timeout_seconds);
        config.supports_custom_temperature = Some(false);
        if let Some(max) = profile.max_output_tokens {
            config.max_tokens = max;
        }
        AnthropicAiProvider::new(config)
            .map(|provider| CodeProvider::Anthropic {
                provider: crate::providers::Anthropic::new(
                    provider,
                    profile.max_output_tokens,
                    profile.context_window,
                    self.diagnostics.clone(),
                )
                .for_agent(&profile.id),
                vision: profile.vision,
            })
            .map_err(|_| "Cannot initialize the Anthropic connection".into())
    }

    fn openai_config(
        &self,
        profile: &Agent,
        provider: &Provider,
    ) -> Result<roomkit_providers_openai::OpenAiConfig, String> {
        let config = match &provider.authentication {
            Authentication::ChatGpt { account } => roomkit_providers_openai::OpenAiConfig {
                token_source: Some(self.accounts.token_source(account)?),
                ..roomkit_providers_openai::OpenAiConfig::new("", &profile.model)
            },
            _ => roomkit_providers_openai::OpenAiConfig {
                base_url: Some(provider.base_url.clone()),
                ..roomkit_providers_openai::OpenAiConfig::new(
                    provider.authentication.key()?,
                    &profile.model,
                )
            },
        };
        // An agent's requests share their prompt's start: one key routes them
        // to one prompt cache. Without it, a turn's later steps mostly missed
        // it too (RMK-1481).
        let config = match provider.subscription() {
            Some(subscription) => subscription.configure(config, &self.workspace, &profile.id),
            None => config,
        };
        Ok(roomkit_providers_openai::OpenAiConfig {
            timeout: std::time::Duration::from_secs(provider.timeout_seconds),
            vision: profile.vision,
            reasoning_effort: profile.reasoning_effort.clone(),
            // An effort is the person's choice, and every turn here has tools:
            // left unstated, the kit would drop it from each of them behind a
            // base URL. On OpenAI's own endpoint its catalogue still decides for
            // the models it tags.
            supports_reasoning_effort_with_tools: Some(true),
            thinking_field: match provider.subscription() {
                Some(subscription) => {
                    subscription.thinking_field(&provider.base_url, &profile.model)
                }
                None => reasoning::thinking_field(&provider.base_url, &profile.model),
            },
            // No cap unless the agent sets one: the endpoint decides how long
            // a reply may be, as each model has its own limit.
            max_tokens: profile.max_output_tokens,
            // The application sends no temperature, as before: the endpoint's
            // default applies, and a model that refuses one is never sent it.
            supports_custom_temperature: Some(false),
            ..config
        })
    }
}

impl Provider {
    pub(super) fn gemini(&self, model: &str, max_tokens: Option<u32>) -> Result<Gemini, String> {
        let mut config = GeminiConfig::new(self.authentication.key()?);
        config.model = model.into();
        config.timeout = Duration::from_secs(self.timeout_seconds);
        if let Some(max) = max_tokens {
            config.max_tokens = max;
        }
        let client = Gemini::new(config).map_err(|_| "Cannot initialize the Gemini connection")?;
        #[cfg(test)]
        let client = GEMINI_SERVER
            .try_with(|address| client.replaying(*address, 0.0))
            .map_err(|_| "Gemini tests require a loopback fixture")?
            .map_err(|_| "Invalid Gemini fixture address")?
            .0;
        Ok(client)
    }

    pub(super) fn openrouter(
        &self,
        model: &str,
        max_tokens: Option<u32>,
        effort: Option<String>,
    ) -> Result<OpenRouter, String> {
        let config = OpenRouterConfig {
            base_url: self.base_url.clone(),
            timeout: Duration::from_secs(self.timeout_seconds),
            max_tokens,
            reasoning_effort: effort,
            supports_custom_temperature: false,
            ..OpenRouterConfig::new(self.authentication.key()?, model)
        };
        OpenRouter::new(config).map_err(|_| "Cannot initialize the OpenRouter connection".into())
    }
}

#[cfg(test)]
tokio::task_local! {
    pub(crate) static GEMINI_SERVER: std::net::SocketAddr;
}
