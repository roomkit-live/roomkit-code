//! Provider refusals remain private, actionable and recoverable in one session.
use super::*;
use crate::providers::ProviderKind;
use std::sync::atomic::{AtomicBool, Ordering};

const SECRET: &str = "private-fixture-key";

struct ProviderFailure {
    base: String,
    healthy: Arc<AtomicBool>,
    server: tokio::task::JoinHandle<()>,
}

impl ProviderFailure {
    async fn start(status: u16, message: &'static str, streamed: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let healthy = Arc::new(AtomicBool::new(false));
        let state = healthy.clone();
        let router = Router::new().route(
            "/v1/messages",
            post(move || {
                let healthy = state.load(Ordering::SeqCst);
                async move { response(status, message, streamed, healthy) }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            base,
            healthy,
            server,
        }
    }
}

impl Drop for ProviderFailure {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn response(status: u16, message: &str, streamed: bool, healthy: bool) -> axum::response::Response {
    use axum::response::IntoResponse;
    if !healthy && !streamed {
        return (
            axum::http::StatusCode::from_u16(status).unwrap(),
            Json(json!({"error":{"message":format!("{message} {SECRET}")}})),
        )
            .into_response();
    }
    if !healthy && streamed && status == 200 {
        return empty_response().into_response();
    }
    let mut events = vec![
        json!({"type":"message_start", "message":{"model":"claude-fable-5-2", "usage":{"input_tokens":20, "output_tokens":1}}}),
        json!({"type":"content_block_start", "index":0, "content_block":{"type":"text", "text":""}}),
        json!({"type":"content_block_delta", "index":0, "delta":{"type":"text_delta", "text":if healthy {"Recovered answer."} else {"Partial answer."}}}),
    ];
    if healthy {
        events.extend([
            json!({"type":"content_block_stop", "index":0}),
            json!({"type":"message_delta", "delta":{"stop_reason":"end_turn"}, "usage":{"output_tokens":10}}),
            json!({"type":"message_stop"}),
        ]);
    } else {
        events.push(json!({"type":"error", "error":{"type":"overloaded_error", "message":format!("{message} {SECRET}")}}));
    }
    let wire = events
        .into_iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect::<String>();
    ([("content-type", "text/event-stream")], wire).into_response()
}

pub(super) async fn submit(app: &Application, operation: &str) -> Vec<Event> {
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    assert!(matches!(event(&client).await, Event::Snapshot(_)));
    client
        .try_send(Request::new(
            RequestId::new(1),
            operation,
            app.view.room.as_str(),
            Command::Submit {
                agent: "code".into(),
                text: "Reply briefly.".into(),
            },
        ))
        .unwrap();
    let mut events = Vec::new();
    loop {
        let next = event(&client).await;
        let done = matches!(next, Event::OperationFinished { .. });
        events.push(next);
        if done {
            break;
        }
    }
    drop(client);
    task.await.unwrap().unwrap();
    events
}

#[tokio::test]
async fn anthropic_errors_reach_the_view_and_log_then_the_session_recovers() {
    for (status, message, code, expected, streamed) in [
        (
            401,
            "Invalid credential",
            "provider_access_denied",
            "access denied",
            false,
        ),
        (
            404,
            "Unknown model",
            "provider_not_found",
            "model not found",
            false,
        ),
        (
            400,
            "output_config.effort: unsupported value",
            "provider_reasoning_rejected",
            "reasoning",
            false,
        ),
        (429, "Rate limit", "provider_quota", "quota", false),
        (
            402,
            "Your credit balance is too low to access the Anthropic API",
            "provider_payment_required",
            "credit",
            false,
        ),
        (529, "Overloaded", "provider_overloaded", "overloaded", true),
    ] {
        let server = ProviderFailure::start(status, message, streamed).await;
        let root = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        let mut settings = settings(root.path(), storage.path(), &server.base);
        settings.provider = ProviderKind::Anthropic;
        settings.model = "claude-fable-5-2".into();
        let app = Application::open(&settings, SECRET.into()).await.unwrap();
        let events = submit(&app, "failed-turn").await;
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::OperationFinished {
                    outcome: RunOutcome::Failed,
                    ..
                }
            )),
            "{events:?}"
        );
        let notices: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::Notice { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            notices.iter().filter(|s| s.contains(expected)).count(),
            1,
            "{notices:?}"
        );
        assert!(!serde_json::to_string(&events).unwrap().contains(SECRET));
        server.healthy.store(true, Ordering::SeqCst);
        let recovered = submit(&app, "resumed-turn").await;
        assert!(
            recovered.iter().any(|e| matches!(
                e,
                Event::OperationFinished {
                    outcome: RunOutcome::Completed,
                    ..
                }
            )),
            "{recovered:?}"
        );
        assert!(
            serde_json::to_string(&recovered)
                .unwrap()
                .contains("Recovered answer.")
        );
        app.close().await.unwrap();
        let log =
            std::fs::read_to_string(app._storage.database.with_file_name("diagnostics.jsonl"))
                .unwrap();
        let records: Vec<serde_json::Value> = log
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let failure = records
            .iter()
            .find(|record| record["code"] == code)
            .unwrap();
        // Display omits the envelope when the message already names Anthropic.
        let expected_status = if status == 402 {
            serde_json::Value::Null
        } else {
            json!(status)
        };
        assert_eq!(failure["http_status"], expected_status, "{log}");
        assert!(!log.contains(SECRET));
        assert!(!log.contains("engine_failed"));
    }
}

/// OpenCode Go refuses a model its organization's region setting excludes
/// (2026-11-12). The person reads Go's own words, which say where to change
/// it, below the explanation; the diagnostics log keeps the code alone.
#[tokio::test]
async fn a_provider_s_refusal_reaches_the_view_in_its_own_words() {
    const WORDS: &str = "This OpenCode Go model is hosted outside Europe and the United States. \
        Select Global at https://opencode.ai/console/org_01ABC/settings/privacy#regions to use it.";
    let router = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"error": {"type": "invalid_request_error", "message": WORDS}})),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let settings = settings(root.path(), storage.path(), &base);
    let app = Application::open(&settings, SECRET.into()).await.unwrap();
    let events = submit(&app, "refused-turn").await;
    let notices: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Notice { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let told = format!(
        "Provider rejected the request format or parameters. Check model and tool-calling \
         support.\nProvider says: {WORDS}"
    );
    assert!(notices.contains(&told.as_str()), "{notices:?}");
    app.close().await.unwrap();
    server.abort();
    let log =
        std::fs::read_to_string(app._storage.database.with_file_name("diagnostics.jsonl")).unwrap();
    assert!(log.contains("provider_request_rejected"), "{log}");
    assert!(!log.contains("hosted outside"), "{log}");
}

fn empty_response() -> ([(&'static str, &'static str); 1], String) {
    let events = [
        json!({"type":"message_start", "message":{"model":"claude-fable-5-2", "usage":{"input_tokens":20, "output_tokens":1}}}),
        json!({"type":"content_block_start", "index":0, "content_block":{"type":"thinking", "thinking":""}}),
        json!({"type":"content_block_delta", "index":0, "delta":{"type":"thinking_delta", "thinking":"Checking the request."}}),
        json!({"type":"content_block_delta", "index":0, "delta":{"type":"signature_delta", "signature":"fixture-signature"}}),
        json!({"type":"content_block_stop", "index":0}),
        json!({"type":"message_delta", "delta":{"stop_reason":"max_tokens"}, "usage":{"output_tokens":1024}}),
        json!({"type":"message_stop"}),
    ];
    let wire = events
        .into_iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    ([("content-type", "text/event-stream")], wire)
}

#[tokio::test]
async fn anthropic_thinking_without_an_answer_is_explained_and_can_be_retried() {
    let server = ProviderFailure::start(200, "", true).await;
    let root = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let mut settings = settings(root.path(), storage.path(), &server.base);
    settings.provider = ProviderKind::Anthropic;
    settings.model = "claude-fable-5-2".into();
    let app = Application::open(&settings, SECRET.into()).await.unwrap();
    let events = submit(&app, "thinking-only").await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::OperationFinished {
                outcome: RunOutcome::Failed,
                ..
            }
        )),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Notice { text, .. } if text.contains("output limit"))),
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e, Event::ContextUpdated { usage, .. } if usage.tokens == 1044
        )),
        "{events:?}"
    );
    server.healthy.store(true, Ordering::SeqCst);
    let recovered = submit(&app, "retried").await;
    assert!(
        serde_json::to_string(&recovered)
            .unwrap()
            .contains("Recovered answer.")
    );
    app.close().await.unwrap();
    let log =
        std::fs::read_to_string(app._storage.database.with_file_name("diagnostics.jsonl")).unwrap();
    assert!(log.contains("provider_empty_response"), "{log}");
}
