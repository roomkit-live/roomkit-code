//! The Responses provider's streamed usage refusal reaches the safe diagnostic.

#![expect(clippy::unwrap_used, reason = "fixture failures must fail the test")]

use super::*;
use axum::{Router, routing::post};
use roomkit_openai::{OpenAiApi, OpenAiConfig};
use roomkit_tui_engine::Diagnostic;
use serde_json::json;

#[tokio::test]
async fn a_streamed_chatgpt_usage_limit_reports_quota_instead_of_bad_parameters() {
    let failure = json!({"type":"response.failed", "response":{
        "status":"failed", "error":{
            "type":"invalid_request_error",
            "code":"subscription_sharing_usage_limit_exceeded",
            "message":"Private account details"
        }
    }});
    let body = format!("event: response.failed\ndata: {failure}\n\n");
    let router = Router::new().route(
        "/v1/responses",
        post(move || async { ([("content-type", "text/event-stream")], body) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let provider = CodeProvider::OpenAi(
        OpenAiAiProvider::new(OpenAiConfig {
            base_url: Some(base_url),
            api: OpenAiApi::Responses,
            ..OpenAiConfig::new("", "fixture-model")
        })
        .unwrap(),
    );
    let context = AiContext::default();
    let mut stream = provider.generate_structured_stream(&context);
    let event = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await;
    server.abort();
    let error = event.unwrap().unwrap_err();
    let diagnostic = Diagnostic::classify(&error.to_string());
    assert_eq!(diagnostic.code, "provider_quota");
    assert!(diagnostic.summary.contains("ChatGPT Settings > Usage"));
    assert!(!diagnostic.summary.contains("Private account details"));
}
