//! Recovery through the real channel: usage, signed tool replay and failed retries.
use super::*;
use crate::providers::ProviderKind;
use axum::response::IntoResponse;

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn start(answer: fn(usize, &Value) -> axum::response::Response) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let saved = requests.clone();
        let router = Router::new().route(
            "/v1/messages",
            post(move |Json(request): Json<Value>| {
                let saved = saved.clone();
                async move {
                    let mut saved = saved.lock().unwrap();
                    let index = saved.len();
                    saved.push(request.clone());
                    answer(index, &request)
                }
            }),
        );
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            url,
            requests,
            task,
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn response(blocks: Vec<Value>, stop: &str, tokens: u32) -> axum::response::Response {
    let mut events = vec![
        json!({"type":"message_start","message":{"model":"claude-fable-5-2","usage":{"input_tokens":20,"output_tokens":0}}}),
    ];
    for (index, block) in blocks.into_iter().enumerate() {
        let (start, delta) = match block["type"].as_str().unwrap() {
            "thinking" => (
                json!({"type":"thinking","thinking":""}),
                json!({"type":"signature_delta","signature":block["signature"]}),
            ),
            "tool_use" => (
                json!({"type":"tool_use","id":block["id"],"name":"read","input":{}}),
                json!({"type":"input_json_delta","partial_json":json!({"path":"fixture.txt","_roomkit_intent":"  Read\n fixture  "}).to_string()}),
            ),
            _ => (
                json!({"type":"text","text":""}),
                json!({"type":"text_delta","text":block["text"]}),
            ),
        };
        events.extend([
            json!({"type":"content_block_start","index":index,"content_block":start}),
            json!({"type":"content_block_delta","index":index,"delta":delta}),
            json!({"type":"content_block_stop","index":index}),
        ]);
    }
    events.extend([
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":tokens}}),
        json!({"type":"message_stop"}),
    ]);
    let body = events
        .into_iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect::<String>();
    ([("content-type", "text/event-stream")], body).into_response()
}
fn thinking(signature: &str) -> Value {
    json!({"type":"thinking","signature":signature})
}
fn tool(id: &str) -> Value {
    json!({"type":"tool_use","id":id})
}
fn answer() -> axum::response::Response {
    response(
        vec![json!({"type":"text","text":"Recovered."})],
        "end_turn",
        10,
    )
}
fn error(message: String) -> axum::response::Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        Json(json!({"error":{"type":"invalid_request_error","message":message}})),
    )
        .into_response()
}
fn blocks(request: &Value) -> impl Iterator<Item = &Value> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["content"].as_array())
        .flatten()
}
fn failure(request: &Value) -> axum::response::Response {
    // Anthropic reports indices after combining consecutive turns of one role.
    let mut grouped = Vec::<Value>::new();
    for message in request["messages"].as_array().unwrap() {
        let content = message["content"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| vec![json!({"type":"text", "text":message["content"]})]);
        if let Some(last) = grouped.last_mut().filter(|m| m["role"] == message["role"]) {
            last["content"].as_array_mut().unwrap().extend(content);
        } else {
            grouped.push(json!({"role":message["role"], "content":content}));
        }
    }
    let (i, j) = grouped
        .iter()
        .enumerate()
        .find_map(|(i, m)| {
            m["content"].as_array().and_then(|p| {
                p.iter()
                    .position(|p| p["type"] == "thinking")
                    .map(|j| (i, j))
            })
        })
        .unwrap();
    error(format!(
        "messages.{i}.content.{j}: Invalid `signature` in `thinking` block. The block is bound to a different conversation."
    ))
}

async fn run(server: &Server, manual: Option<u32>) -> (Vec<Event>, String) {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("fixture.txt"), "safe fixture").unwrap();
    let mut settings = settings(root.path(), state.path(), &server.url);
    settings.provider = ProviderKind::Anthropic;
    settings.model = "claude-fable-5-2".into();
    settings.max_output_tokens = manual;
    let app = Application::open(&settings, "fixture-key".into())
        .await
        .unwrap();
    let events = super::provider_errors::submit(&app, "recovery").await;
    app.close().await.unwrap();
    let log =
        std::fs::read_to_string(app._storage.database.with_file_name("diagnostics.jsonl")).unwrap();
    (events, log)
}
fn completed(events: &[Event]) {
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::OperationFinished {
                outcome: RunOutcome::Completed,
                ..
            }
        )),
        "{events:?}"
    );
}

#[tokio::test]
async fn auto_grows_once_keeps_usage_and_never_repeats_a_tool() {
    let server = Server::start(|i, r| match i {
        0 => response(vec![thinking("discarded")], "max_tokens", 32000),
        1 => response(vec![thinking("kept"), tool("one")], "tool_use", 30),
        _ => {
            assert!(!r.to_string().contains("discarded"));
            assert!(r.to_string().contains("kept"));
            assert_eq!(
                blocks(r).find(|p| p["type"] == "tool_use").unwrap()["input"]["_roomkit_intent"],
                "  Read\n fixture  "
            );
            answer()
        }
    })
    .await;
    let (events, log) = run(&server, None).await;
    completed(&events);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::UsageUpdated { usage, .. }
        if usage.input_tokens == Some(60) && usage.output_tokens == Some(32040)))
    );
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .map(|r| r["max_tokens"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [32000, 64000, 64000]
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ContextUpdated { usage, .. } if usage.tokens == 50))
    );
    assert!(log.contains("output_budget"));
    assert_eq!(log.matches("\"event\":\"tool_finished\"").count(), 1);
}

#[tokio::test]
async fn rejected_thinking_stays_removed_while_new_signed_rounds_are_preserved() {
    let server = Server::start(|i, r| match i {
        0 => response(vec![thinking("bad"), tool("one")], "tool_use", 30),
        1 => failure(r),
        2 => {
            assert!(!blocks(r).any(|p| p["type"] == "thinking"));
            assert!(blocks(r).any(|p| p["type"] == "tool_result"));
            response(vec![thinking("new"), tool("two")], "tool_use", 30)
        }
        _ => {
            assert!(!r.to_string().contains("\"bad\""));
            assert!(r.to_string().contains("\"new\""));
            answer()
        }
    })
    .await;
    let (events, log) = run(&server, Some(8192)).await;
    completed(&events);
    assert_eq!(server.requests.lock().unwrap().len(), 4);
    assert_eq!(log.matches("\"event\":\"tool_finished\"").count(), 2);
    assert!(log.contains("reasoning_replay"));
    assert!(!log.contains("signature"));
}

#[tokio::test]
async fn auto_retry_failure_is_failed_and_its_known_usage_is_logged() {
    let server = Server::start(|i, _| {
        if i == 0 {
            response(vec![thinking("discarded")], "max_tokens", 32000)
        } else {
            error("Unrelated invalid request".into())
        }
    })
    .await;
    let (events, log) = run(&server, None).await;
    assert!(events.iter().any(|e| matches!(
        e,
        Event::OperationFinished {
            outcome: RunOutcome::Failed,
            ..
        }
    )));
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    assert!(
        log.lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .any(|r| r["reason"] == "output_budget" && r["usage"]["output_tokens"] == 32000)
    );
}

#[tokio::test]
async fn manual_budget_and_visible_output_never_trigger_budget_recovery() {
    for (manual, visible) in [(Some(8192), false), (None, true)] {
        let server = Server::start(if visible {
            |_, _| {
                response(
                    vec![json!({"type":"text","text":"Partial answer"})],
                    "max_tokens",
                    32000,
                )
            }
        } else {
            |_, _| response(vec![thinking("manual")], "max_tokens", 8192)
        })
        .await;
        let (_, log) = run(&server, manual).await;
        // The engine may nudge a cut generation independently, but our policy
        // must never grow a fixed limit or replay already displayed output.
        assert!(!log.contains("output_budget"));
        assert!(
            server
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|r| r["max_tokens"] == manual.unwrap_or(32000))
        );
    }
}

#[tokio::test]
async fn repeated_exhaustion_and_repeated_signature_rejection_stay_bounded() {
    let server = Server::start(|_, _| response(vec![thinking("cut")], "max_tokens", 32000)).await;
    let (events, log) = run(&server, None).await;
    assert!(events.iter().any(|e| matches!(
        e,
        Event::OperationFinished {
            outcome: RunOutcome::Failed,
            ..
        }
    )));
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    assert_eq!(log.matches("\"reason\":\"output_budget\"").count(), 1);
    let server = Server::start(|i, r| match i {
        0 => response(vec![thinking("bad"), tool("one")], "tool_use", 30),
        1 => failure(r),
        2 => response(vec![thinking("also-bad"), tool("two")], "tool_use", 30),
        _ => failure(r),
    })
    .await;
    let (events, log) = run(&server, None).await;
    assert!(events.iter().any(|e| matches!(
        e,
        Event::OperationFinished {
            outcome: RunOutcome::Failed,
            ..
        }
    )));
    assert_eq!(server.requests.lock().unwrap().len(), 4);
    assert_eq!(log.matches("\"reason\":\"reasoning_replay\"").count(), 1);
}

#[tokio::test]
async fn dropping_the_stream_before_the_retry_prevents_another_request() {
    use roomkit_ai::{AiContext, AiMessage, AiProvider, AiRole, StreamEvent};
    use roomkit_anthropic::{AnthropicAiProvider, AnthropicConfig};
    let server = Server::start(|_, _| response(vec![thinking("cut")], "max_tokens", 32000)).await;
    let temp = tempfile::tempdir().unwrap();
    let diagnostics =
        crate::diagnostics::Diagnostics::open(temp.path().join("diagnostics.jsonl"), false)
            .unwrap();
    let mut config = AnthropicConfig::new("fixture-key", "claude-fable-5-2");
    config.base_url = Some(server.url.clone());
    let native = crate::providers::Anthropic::new(
        AnthropicAiProvider::new(config).unwrap(),
        None,
        None,
        diagnostics,
    );
    let provider = crate::providers::CodeProvider::Anthropic {
        provider: native,
        vision: false,
    };
    let context = AiContext {
        messages: vec![AiMessage::new(AiRole::User, "Reply")],
        ..AiContext::default()
    };
    let mut stream = provider.generate_structured_stream(&context);
    let first = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await;
    assert!(matches!(first, Some(Ok(StreamEvent::Done(_)))));
    drop(stream);
    tokio::task::yield_now().await;
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}
