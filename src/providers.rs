//! Application provider selection, shared by launch settings and saved profiles.
use roomkit_ai::{
    AiContext, AiProvider, AiResponse, ModelInfo, ProviderError, ProviderStream, TextStream,
};
mod anthropic;
pub(crate) use anthropic::Anthropic;
use roomkit_providers_anthropic::Anthropic as AnthropicAiProvider;
use roomkit_providers_gemini::Gemini;
use roomkit_providers_openai::{OpenAi as OpenAiAiProvider, OpenRouter};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod openai_tests;

/// Wire protocol of a connection. Older profiles use OpenAI-compatible requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ProviderKind {
    #[default]
    #[value(name = "openai")]
    OpenAi,
    Anthropic,
    Gemini,
    #[value(name = "openrouter")]
    OpenRouter,
    #[value(name = "llamacpp")]
    LlamaCpp,
}

impl ProviderKind {
    pub fn value(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::OpenRouter => "openrouter",
            Self::LlamaCpp => "llamacpp",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAIAIProvider",
            Self::Gemini => "GeminiAIProvider",
            Self::OpenRouter => "OpenRouterAIProvider",
            Self::Anthropic => "AnthropicAIProvider",
            Self::LlamaCpp => "llamacpp",
        }
    }

    pub fn default_url(self) -> &'static str {
        match self {
            Self::OpenAi | Self::LlamaCpp => "",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::Anthropic => "https://api.anthropic.com",
        }
    }

    /// The native Gemini client always sends requests to Google's Developer API.
    pub fn check_endpoint(self, base_url: &str) -> Result<(), String> {
        if self == Self::Gemini && base_url.trim_end_matches('/') != self.default_url() {
            return Err(format!(
                "Gemini connections use the Developer API at {}",
                self.default_url()
            ));
        }
        Ok(())
    }

    pub fn models(self) -> Vec<ModelInfo> {
        match self {
            Self::OpenAi => OpenAiAiProvider::available_models(),
            Self::Anthropic => AnthropicAiProvider::available_models(),
            Self::Gemini => Gemini::available_models(),
            Self::OpenRouter => OpenRouter::available_models(),
            Self::LlamaCpp => Vec::new(),
        }
    }
}

/// The same channel and compaction code serve every provider.
#[derive(Clone)]
pub(crate) enum CodeProvider {
    OpenAi(OpenAiAiProvider),
    Gemini { provider: Gemini, vision: bool },
    OpenRouter { provider: OpenRouter, vision: bool },
    Local(crate::local_models::Local),
    Anthropic { provider: Anthropic, vision: bool },
}

impl AiProvider for CodeProvider {
    fn name(&self) -> &str {
        match self {
            Self::OpenAi(p) => p.name(),
            Self::Gemini { provider: p, .. } => p.name(),
            Self::OpenRouter { provider: p, .. } => p.name(),
            Self::Local(p) => p.inner.name(),
            Self::Anthropic { provider: p, .. } => p.inner.name(),
        }
    }

    fn model_name(&self) -> &str {
        match self {
            Self::OpenAi(p) => p.model_name(),
            Self::Gemini { provider: p, .. } => p.model_name(),
            Self::OpenRouter { provider: p, .. } => p.model_name(),
            Self::Local(p) => &p.model,
            Self::Anthropic { provider: p, .. } => p.inner.model_name(),
        }
    }

    fn catalog_entry(&self) -> Option<ModelInfo> {
        match self {
            Self::OpenAi(p) => p.catalog_entry(),
            Self::Gemini { provider: p, .. } => p.catalog_entry(),
            Self::OpenRouter { provider: p, .. } => p.catalog_entry(),
            Self::Local(p) => p.inner.catalog_entry(),
            Self::Anthropic { provider: p, .. } => p.inner.catalog_entry(),
        }
    }

    fn supports_vision(&self) -> bool {
        match self {
            Self::OpenAi(p) => p.supports_vision(),
            Self::Gemini { vision, .. } => *vision,
            Self::OpenRouter { provider, vision } => *vision && provider.supports_vision(),
            Self::Local(_) => false,
            // The agent setting explicitly enables images, even for a gateway alias.
            Self::Anthropic { vision, .. } => *vision,
        }
    }

    fn supports_deferred_tools(&self) -> bool {
        match self {
            Self::OpenAi(p) => p.supports_deferred_tools(),
            Self::Gemini { provider: p, .. } => p.supports_deferred_tools(),
            Self::OpenRouter { provider: p, .. } => p.supports_deferred_tools(),
            Self::Local(p) => p.inner.supports_deferred_tools(),
            Self::Anthropic { provider: p, .. } => p.inner.supports_deferred_tools(),
        }
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_structured_streaming(&self) -> bool {
        true
    }

    fn supports_response_schema(&self) -> bool {
        match self {
            Self::OpenAi(p) => p.supports_response_schema(),
            Self::Gemini { provider: p, .. } => p.supports_response_schema(),
            Self::OpenRouter { provider: p, .. } => p.supports_response_schema(),
            Self::Local(p) => p.inner.supports_response_schema(),
            Self::Anthropic { provider: p, .. } => p.inner.supports_response_schema(),
        }
    }

    fn supports_response_schema_with_tools(&self) -> bool {
        match self {
            Self::OpenAi(p) => p.supports_response_schema_with_tools(),
            Self::Gemini { provider: p, .. } => p.supports_response_schema_with_tools(),
            Self::OpenRouter { provider: p, .. } => p.supports_response_schema_with_tools(),
            Self::Local(p) => p.inner.supports_response_schema_with_tools(),
            Self::Anthropic { provider: p, .. } => p.inner.supports_response_schema_with_tools(),
        }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        match self {
            Self::OpenAi(p) => p.list_models().await,
            Self::Gemini { provider: p, .. } => p.list_models().await,
            Self::OpenRouter { provider: p, .. } => p.list_models().await,
            Self::Local(p) => p.list_models().await,
            Self::Anthropic { provider: p, .. } => p.inner.list_models().await,
        }
    }

    async fn generate(&self, context: &AiContext) -> Result<AiResponse, ProviderError> {
        match self {
            Self::OpenAi(p) => p.generate(context).await,
            Self::Gemini { provider: p, .. } => p.generate(context).await,
            Self::OpenRouter { provider: p, .. } => p.generate(context).await,
            Self::Local(p) => p.generate(context).await,
            Self::Anthropic { provider: p, .. } => p.generate(context).await,
        }
    }

    fn generate_stream<'a>(&'a self, context: &'a AiContext) -> TextStream<'a> {
        match self {
            Self::OpenAi(p) => p.generate_stream(context),
            Self::Gemini { provider: p, .. } => p.generate_stream(context),
            Self::OpenRouter { provider: p, .. } => p.generate_stream(context),
            Self::Local(p) => p.generate_stream(context),
            Self::Anthropic { provider: p, .. } => p.generate_stream(context),
        }
    }

    fn generate_structured_stream<'a>(&'a self, context: &'a AiContext) -> ProviderStream<'a> {
        match self {
            Self::OpenAi(p) => p.generate_structured_stream(context),
            Self::Gemini { provider: p, .. } => p.generate_structured_stream(context),
            Self::OpenRouter { provider: p, .. } => p.generate_structured_stream(context),
            Self::Local(p) => p.generate_structured_stream(context),
            Self::Anthropic { provider: p, .. } => p.generate_structured_stream(context),
        }
    }
}
