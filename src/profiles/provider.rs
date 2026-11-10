//! Construct a provider without mixing its wire settings with agent assembly.
use super::*;
use crate::providers::{CodeProvider, ProviderKind};
use roomkit_anthropic::{AnthropicAiProvider, AnthropicConfig};
use roomkit_openai::OpenAiAiProvider;

impl Profiles {
    pub(super) fn generation_provider(
        &self,
        profile: &Agent,
        provider: &Provider,
    ) -> Result<CodeProvider, String> {
        match provider.kind {
            ProviderKind::OpenAi => OpenAiAiProvider::new(self.openai_config(profile, provider)?)
                .map(CodeProvider::OpenAi)
                .map_err(|_| "Cannot initialize the OpenAI connection".into()),
            ProviderKind::Anthropic => {
                let mut config =
                    AnthropicConfig::new(provider.authentication.key()?, &profile.model);
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
        }
    }

    fn openai_config(
        &self,
        profile: &Agent,
        provider: &Provider,
    ) -> Result<roomkit_openai::OpenAiConfig, String> {
        let config = match &provider.authentication {
            Authentication::ChatGpt { account } => roomkit_openai::OpenAiConfig {
                token_source: Some(self.accounts.token_source(account)?),
                ..roomkit_openai::OpenAiConfig::new("", &profile.model)
            },
            _ => roomkit_openai::OpenAiConfig {
                base_url: Some(provider.base_url.clone()),
                ..roomkit_openai::OpenAiConfig::new(provider.authentication.key()?, &profile.model)
            },
        };
        // An agent's requests share their prompt's start: one key routes them
        // to one prompt cache. Without it, a turn's later steps mostly missed
        // it too (RMK-1481).
        let config = match provider.subscription() {
            Some(subscription) => subscription.configure(config, &self.workspace, &profile.id),
            None => config,
        };
        Ok(roomkit_openai::OpenAiConfig {
            timeout: std::time::Duration::from_secs(provider.timeout_seconds),
            vision: profile.vision,
            reasoning_effort: profile.reasoning_effort.clone(),
            // An effort is the person's choice, and every turn here has tools:
            // left unstated, the kit would drop it from each of them behind a
            // base URL. On OpenAI's own endpoint its catalogue still decides for
            // the models it tags.
            supports_reasoning_effort_with_tools: Some(true),
            thinking_field: reasoning::thinking_field(&provider.base_url, &profile.model),
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
