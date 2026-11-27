//! Apply project recall to the request the engine actually sends, including every tool round.
use super::View;
use crate::providers::CodeProvider;
use roomkit_ai::{
    AiContext, AiProvider, AiResponse, ModelInfo, ProviderError, ProviderStream, TextStream,
};
use std::{borrow::Cow, future::poll_fn};

#[derive(Clone)]
pub(crate) struct AgentProvider {
    inner: CodeProvider,
    view: Option<View>,
}
impl AgentProvider {
    pub fn new(inner: CodeProvider, view: Option<View>) -> Self {
        Self { inner, view }
    }
    async fn request<'a>(&self, ctx: &'a AiContext) -> Cow<'a, AiContext> {
        match &self.view {
            Some(v) => v.prepare(ctx).await,
            None => Cow::Borrowed(ctx),
        }
    }
}
impl AiProvider for AgentProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn model_name(&self) -> &str {
        self.inner.model_name()
    }
    fn catalog_entry(&self) -> Option<ModelInfo> {
        self.inner.catalog_entry()
    }
    fn supports_vision(&self) -> bool {
        self.inner.supports_vision()
    }
    fn supports_deferred_tools(&self) -> bool {
        self.inner.supports_deferred_tools()
    }
    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }
    fn supports_structured_streaming(&self) -> bool {
        self.inner.supports_structured_streaming()
    }
    fn supports_response_schema(&self) -> bool {
        self.inner.supports_response_schema()
    }
    fn supports_response_schema_with_tools(&self) -> bool {
        self.inner.supports_response_schema_with_tools()
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        self.inner.list_models().await
    }
    async fn generate(&self, ctx: &AiContext) -> Result<AiResponse, ProviderError> {
        self.inner.generate(self.request(ctx).await.as_ref()).await
    }
    fn generate_stream<'a>(&'a self, ctx: &'a AiContext) -> TextStream<'a> {
        Box::pin(async_stream::stream! {
            let request=self.request(ctx).await;
            let mut stream=self.inner.generate_stream(&request);
            while let Some(item)=poll_fn(|cx|stream.as_mut().poll_next(cx)).await {yield item;}
        })
    }
    fn generate_structured_stream<'a>(&'a self, ctx: &'a AiContext) -> ProviderStream<'a> {
        Box::pin(async_stream::stream! {
            let request=self.request(ctx).await;
            let mut stream=self.inner.generate_structured_stream(&request);
            while let Some(item)=poll_fn(|cx|stream.as_mut().poll_next(cx)).await {yield item;}
        })
    }
    async fn close(&self) {
        self.inner.close().await;
    }
}
