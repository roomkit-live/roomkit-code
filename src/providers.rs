//! Application provider selection, shared by launch settings and saved profiles.
use roomkit_ai::{
    AiContext, AiProvider, AiResponse, ModelInfo, ProviderError, ProviderStream, TextStream,
};
use roomkit_anthropic::AnthropicAiProvider;
use roomkit_openai::OpenAiAiProvider;
use serde::{Deserialize, Serialize};

/// Wire protocol of a connection. Older profiles use OpenAI-compatible requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ProviderKind {
    #[default]
    #[value(name = "openai")]
    OpenAi,
    Anthropic,
}

impl ProviderKind {
    pub fn value(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAIAIProvider",
            Self::Anthropic => "AnthropicAIProvider",
        }
    }

    pub fn default_url(self) -> &'static str {
        match self {
            Self::OpenAi => "",
            Self::Anthropic => "https://api.anthropic.com",
        }
    }

    pub fn models(self) -> Vec<ModelInfo> {
        match self {
            Self::OpenAi => OpenAiAiProvider::available_models(),
            Self::Anthropic => AnthropicAiProvider::available_models(),
        }
    }
}

/// The same channel and compaction code serve either native provider.
#[derive(Clone)]
pub(crate) enum CodeProvider {
    OpenAi(OpenAiAiProvider),
    Anthropic {
        provider: AnthropicAiProvider,
        vision: bool,
    },
}

impl AiProvider for CodeProvider {
    fn name(&self) -> &str {
        match self {
            Self::OpenAi(p) => p.name(),
            Self::Anthropic { provider: p, .. } => p.name(),
        }
    }

    fn model_name(&self) -> &str {
        match self {
            Self::OpenAi(p) => p.model_name(),
            Self::Anthropic { provider: p, .. } => p.model_name(),
        }
    }

    fn catalog_entry(&self) -> Option<ModelInfo> {
        match self {
            Self::OpenAi(p) => p.catalog_entry(),
            Self::Anthropic { provider: p, .. } => p.catalog_entry(),
        }
    }

    fn supports_vision(&self) -> bool {
        match self {
            Self::OpenAi(p) => p.supports_vision(),
            // The agent setting explicitly enables images, even for a gateway alias.
            Self::Anthropic { vision, .. } => *vision,
        }
    }

    fn supports_deferred_tools(&self) -> bool {
        match self {
            Self::OpenAi(p) => p.supports_deferred_tools(),
            Self::Anthropic { provider: p, .. } => p.supports_deferred_tools(),
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
            Self::Anthropic { provider: p, .. } => p.supports_response_schema(),
        }
    }

    fn supports_response_schema_with_tools(&self) -> bool {
        match self {
            Self::OpenAi(p) => p.supports_response_schema_with_tools(),
            Self::Anthropic { provider: p, .. } => p.supports_response_schema_with_tools(),
        }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        match self {
            Self::OpenAi(p) => p.list_models().await,
            Self::Anthropic { provider: p, .. } => p.list_models().await,
        }
    }

    async fn generate(&self, context: &AiContext) -> Result<AiResponse, ProviderError> {
        match self {
            Self::OpenAi(p) => p.generate(context).await,
            Self::Anthropic { provider: p, .. } => p.generate(context).await,
        }
    }

    fn generate_stream<'a>(&'a self, context: &'a AiContext) -> TextStream<'a> {
        match self {
            Self::OpenAi(p) => p.generate_stream(context),
            Self::Anthropic { provider: p, .. } => p.generate_stream(context),
        }
    }

    fn generate_structured_stream<'a>(&'a self, context: &'a AiContext) -> ProviderStream<'a> {
        match self {
            Self::OpenAi(p) => p.generate_structured_stream(context),
            Self::Anthropic { provider: p, .. } => p.generate_structured_stream(context),
        }
    }
}
