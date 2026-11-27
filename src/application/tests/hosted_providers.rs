//! Hosted provider profiles exercised against loopback model endpoints.
use super::*;
use crate::providers::ProviderKind;
use axum::{http::HeaderMap, routing::get};

async fn choose_and_run(kind: ProviderKind, url: &str, model: &str) {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut settings = settings(root.path(), state.path(), "http://localhost");
    settings.base_url.clear();
    settings.model.clear();
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    add_connection(
        &app,
        &[
            ("id", "hosted"),
            ("kind", kind.value()),
            ("url", url),
            ("auth", "stored"),
            ("key", "fixture-key"),
        ],
    )
    .await;
    let models = profile_command(
        &app,
        Command::Invoke {
            name: "providers".into(),
            arguments: "test:hosted".into(),
        },
    )
    .await
    .unwrap();
    let roomkit_tui_engine::CommandResult::Events(events) = models else {
        panic!("expected listed models")
    };
    let listed = serde_json::to_string(&events).unwrap();
    assert!(listed.contains(model), "{listed}");
    assert!(!listed.contains("fixture-key"));
    edit_profile(
        &mut app,
        "agents",
        "add",
        &[
            ("id", "hosted-agent"),
            ("provider", "hosted"),
            ("model", model),
            ("output", "4096"),
            ("effort", "high"),
            ("tools", "read"),
        ],
    )
    .await;
    let session = turn(&app).await;
    assert_eq!(
        session.snapshot().unwrap().model.as_ref().unwrap().provider,
        kind.name()
    );
    app.close().await.unwrap();
}

#[tokio::test]
async fn openrouter_profile_lists_models_and_sends_routed_tool_requests() {
    let mock = Mock::start(vec![]).await;
    choose_and_run(ProviderKind::OpenRouter, &mock.base_url, "discovered-model").await;
    let request = &mock.requests.lock().unwrap()[0];
    assert_eq!(mock.headers.lock().unwrap()[0], "Bearer fixture-key");
    assert_eq!(request["model"], "discovered-model");
    assert_eq!(request["max_tokens"], 4096);
    assert_eq!(request["reasoning"]["effort"], "high");
    assert!(request["tools"].is_array());
    assert!(request.get("temperature").is_none());
}

#[tokio::test]
async fn gemini_profile_lists_models_and_sends_native_streaming_requests() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let model = "gemini-3.8-flash";
    let router = Router::new()
        .route(
            "/v1beta/models",
            get(move |headers: HeaderMap| async move {
                assert_eq!(headers["x-goog-api-key"], "fixture-key");
                Json(json!({"models":[{
                    "name":format!("models/{model}"), "inputTokenLimit":32768,
                    "supportedGenerationMethods":["generateContent"]
                }]}))
            }),
        )
        .route(
            "/v1beta/models/gemini-3.8-flash:streamGenerateContent",
            post(gemini_answer),
        );
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    crate::profiles::GEMINI_SERVER
        .scope(address, choose_and_run(ProviderKind::Gemini, "", model))
        .await;
    task.abort();
}

async fn gemini_answer(
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    assert_eq!(headers["x-goog-api-key"], "fixture-key");
    assert!(!headers.contains_key("authorization"));
    assert_eq!(body["generationConfig"]["maxOutputTokens"], 4096);
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["thinking_level"],
        "HIGH"
    );
    assert!(body["tools"].is_array());
    let chunk = json!({"candidates":[{"content":{"role":"model","parts":[{"text":"Project inspected."}]},"finishReason":"STOP"}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\n"),
    )
}
