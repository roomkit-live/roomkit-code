//! A local provider prepares its optional fork on the first request.
use roomkit_ai::{
    AiContext, AiProvider, AiResponse, ModelInfo, ProviderError, ProviderErrorOptions,
    ProviderStream, TextStream,
};
use roomkit_providers_llamacpp::LlamaCpp;
use std::{future::poll_fn, path::PathBuf, sync::Arc};

#[derive(Clone)]
pub(crate) struct Local {
    pub(super) report: super::observation::Reporter,
    pub(super) active: super::ActiveServer,
    pub(super) max_tokens: Option<u32>,
    pub(super) stop: tokio_util::sync::CancellationToken,
    pub(crate) model: String,
    pub(crate) inner: Arc<LlamaCpp>,
    pub(super) fork: Option<PathBuf>,
}

impl Local {
    async fn ready(&self) -> Result<(), ProviderError> {
        if let Some(path) = &self.fork {
            tokio::select! {
                result=super::runtime::ensure(path)=>result,
                ()=self.stop.cancelled()=>Err("Local model preparation was cancelled".into()),
            }
            .map_err(|e| {
                ProviderError::new(
                    e,
                    ProviderErrorOptions {
                        provider: "llamacpp".into(),
                        ..Default::default()
                    },
                )
            })?;
        }
        Ok(())
    }

    // Requests to the same model can overlap. Switching waits for active readers before
    // releasing the previous child's weights and admitting the first request to the next.
    async fn acquire(
        &self,
    ) -> Result<tokio::sync::OwnedRwLockReadGuard<Option<Arc<LlamaCpp>>>, ProviderError> {
        self.ready().await?;
        let read = self.active.clone().read_owned().await;
        if read.as_ref().is_some_and(|s| Arc::ptr_eq(s, &self.inner)) {
            return Ok(read);
        }
        drop(read);
        let mut active = self.active.clone().write_owned().await;
        if self.stop.is_cancelled() {
            return Err(ProviderError::new(
                "Local inference is shutting down",
                ProviderErrorOptions {
                    provider: "llamacpp".into(),
                    ..Default::default()
                },
            ));
        }
        if !active.as_ref().is_some_and(|s| Arc::ptr_eq(s, &self.inner)) {
            if let Some(previous) = active.as_ref() {
                previous.close().await;
            }
            *active = Some(self.inner.clone());
        }
        Ok(active.downgrade())
    }

    fn request<'a>(&self, request: &'a AiContext) -> std::borrow::Cow<'a, AiContext> {
        if request.max_tokens.unwrap_or(0) == 0
            && let Some(max) = self.max_tokens
        {
            let mut request = request.clone();
            request.max_tokens = Some(max);
            return std::borrow::Cow::Owned(request);
        }
        std::borrow::Cow::Borrowed(request)
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let _server = self.acquire().await?;
        self.inner.list_models().await
    }

    pub async fn generate(&self, context: &AiContext) -> Result<AiResponse, ProviderError> {
        let _server = self.acquire().await?;
        let request = self.request(context);
        let response = self.inner.generate(&request).await?;
        self.report.whole(&request, &response);
        Ok(response)
    }

    pub fn generate_structured_stream<'a>(&'a self, context: &'a AiContext) -> ProviderStream<'a> {
        Box::pin(async_stream::try_stream! {
            let _server = self.acquire().await?;
            let request = self.request(context);
            let mut stream = self.inner.generate_structured_stream(&request);
            let mut observed = super::observation::Output::default();
            while let Some(part) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
                let event = part?;
                self.report.event(&request, &mut observed, &event);
                yield event;
            }
        })
    }

    pub fn generate_stream<'a>(&'a self, context: &'a AiContext) -> TextStream<'a> {
        Box::pin(async_stream::try_stream! {
            let mut stream = self.generate_structured_stream(context);
            while let Some(part) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
                if let roomkit_ai::StreamEvent::TextDelta(delta) = part? {
                    yield delta.text;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "fixture failures should stop the test")]
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tokio::time::{Duration, timeout};

    #[tokio::test]
    async fn switching_gguf_waits_for_requests_and_releases_the_old_server() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("fixture-server");
        std::fs::write(
            &binary,
            r#"#!/usr/bin/env python3
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
class Health(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
HTTPServer(("127.0.0.1", int(sys.argv[sys.argv.index("--port") + 1])), Health).serve_forever()
"#,
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut cfg = roomkit_providers_llamacpp::LlamaCppConfig::new("example/fixture");
        cfg.binary = Some(binary);
        let first = Local {
            report: super::super::observation::Reporter::new(
                crate::diagnostics::Diagnostics::open(dir.path().join("diagnostics.jsonl"), false)
                    .unwrap(),
                "local",
                cfg.context_size,
                cfg.max_tokens,
            ),
            active: Default::default(),
            max_tokens: None,
            model: cfg.model.clone(),
            inner: Arc::new(LlamaCpp::new(cfg.clone()).unwrap()),
            fork: None,
            stop: Default::default(),
        };
        let second = Local {
            inner: Arc::new(LlamaCpp::new(cfg).unwrap()),
            ..first.clone()
        };
        let in_flight = first.acquire().await.unwrap();
        first.inner.start().await.unwrap();
        let url = url::Url::parse(&first.inner.base_url()).unwrap();
        let address = ("127.0.0.1", url.port().unwrap());
        assert!(tokio::net::TcpStream::connect(address).await.is_ok());
        assert!(
            timeout(Duration::from_millis(40), second.acquire())
                .await
                .is_err()
        );
        drop(in_flight);
        let _next = second.acquire().await.unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }
}
