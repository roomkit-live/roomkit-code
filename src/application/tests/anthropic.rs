//! Native Messages requests through profiles, tools, history and compaction.
use super::*;
use crate::providers::ProviderKind;
use roomkit_tui_engine::{CommandHandler, CommandResult};

struct Native {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Native {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let router = Router::new()
            .route("/v1/messages", post(native_answer))
            .route(
                "/v1/models",
                axum::routing::get(|headers: axum::http::HeaderMap| async move {
                    assert_eq!(headers["x-api-key"], "native-fixture-key");
                    assert_eq!(headers["anthropic-version"], "2023-06-01");
                    assert!(!headers.contains_key("authorization"));
                    Json(json!({"data":[{"id":"claude-sonnet-5-5", "display_name":"Claude"}]}))
                }),
            )
            .with_state(requests.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            url,
            requests,
            task,
        }
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn native_answer(
    State(requests): State<Arc<Mutex<Vec<Value>>>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    assert_eq!(headers["x-api-key"], "native-fixture-key");
    assert!(!headers.contains_key("authorization"));
    let mut requests = requests.lock().unwrap();
    let first = requests.is_empty();
    requests.push(request.clone());
    let summary = request["system"]
        .to_string()
        .contains("You summarize a conversation");
    let block = if first {
        json!({"type":"tool_use", "id":"native-call", "name":"read", "input":{}})
    } else {
        json!({"type":"text", "text":""})
    };
    let delta = if first {
        json!({"type":"input_json_delta", "partial_json":json!({"path":"fixture.txt", "_roomkit_intent":"Read the fixture"}).to_string()})
    } else {
        json!({"type":"text_delta", "text":if summary { "Native summary." } else { "Native project inspection complete." }})
    };
    let events = [
        json!({"type":"message_start", "message":{"model":"claude-sonnet-5-5", "usage":{"input_tokens":120, "output_tokens":1, "cache_read_input_tokens":20}}}),
        json!({"type":"content_block_start", "index":0, "content_block":block}),
        json!({"type":"content_block_delta", "index":0, "delta":delta}),
        json!({"type":"content_block_stop", "index":0}),
        json!({"type":"message_delta", "delta":{"stop_reason":if first {"tool_use"} else {"end_turn"}}, "usage":{"output_tokens":10}}),
        json!({"type":"message_stop"}),
    ];
    let wire = events
        .into_iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect::<String>();
    ([("content-type", "text/event-stream")], wire)
}

#[tokio::test]
async fn anthropic_profiles_discover_call_tools_compact_and_resume() {
    let native = Native::start().await;
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("fixture.txt"), "native file content").unwrap();
    let mut settings = settings(root.path(), state.path(), &native.url);
    settings.base_url.clear();
    settings.model.clear();
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    edit_profile(
        &mut app,
        "providers",
        "add",
        &[
            ("id", "native"),
            ("kind", "anthropic"),
            ("url", &native.url),
            ("auth", "stored"),
            ("key", "native-fixture-key"),
        ],
    )
    .await;
    let discovered = profile_command(
        &app,
        Command::Invoke {
            name: "providers".into(),
            arguments: "test:native".into(),
        },
    )
    .await
    .unwrap();
    let CommandResult::Events(events) = discovered else {
        panic!("expected models")
    };
    let text = serde_json::to_string(&events).unwrap();
    assert!(text.contains("claude-sonnet-5-5"));
    assert!(!text.contains("native-fixture-key"));
    edit_profile(
        &mut app,
        "agents",
        "add",
        &[
            ("id", "native-agent"),
            ("provider", "native"),
            ("model", "claude-sonnet-5-5"),
            ("context", ""), // The native catalogue supplies the compaction window.
            ("output", "2048"),
            ("effort", "high"),
            ("tools", "read"),
        ],
    )
    .await;
    let session = turn(&app).await;
    assert_eq!(
        session.snapshot().unwrap().model.as_ref().unwrap().provider,
        ProviderKind::Anthropic.name()
    );
    assert!(session.snapshot().unwrap().items.iter().any(|i| matches!(i, Item::Tool { call, .. } if call.name=="read" && call.state==ToolState::Succeeded)));
    assert!(app.view.pricing.is_some());
    let requests = native.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["max_tokens"], 2048);
    assert_eq!(requests[0]["output_config"]["effort"], "high");
    assert!(requests[0].get("temperature").is_none());
    assert!(requests[0].get("prompt_cache_key").is_none());
    assert!(requests[0]["tools"][0].get("input_schema").is_some());
    assert!(
        requests[1]["messages"]
            .to_string()
            .contains("native file content")
    );
    assert!(requests[1]["messages"].to_string().contains("tool_result"));

    turn_operation(&app, "second").await;
    turn_operation(&app, "third").await;

    // Compaction uses the selected provider too, with its own output budget.
    let commands = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: settings.workspace.clone(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
        services: app.services.clone(),
    };
    commands
        .handle(
            Request::new(
                RequestId::new(10),
                "compact",
                app.view.room.as_str(),
                Command::Invoke {
                    name: "compact".into(),
                    arguments: String::new(),
                },
            ),
            app.view.clone(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while app
            .profiles
            .resources
            .compactions
            .get(app.view.room.as_str(), "native-agent")
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(native.requests.lock().unwrap().iter().any(|r| {
        r["max_tokens"] == 2000
            && r["system"]
                .to_string()
                .contains("You summarize a conversation")
    }));
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, "ignored-bootstrap-key".into())
        .await
        .unwrap();
    assert_eq!(
        app.view.model.as_ref().unwrap().provider,
        ProviderKind::Anthropic.name()
    );
    turn_operation(&app, "native-resumed").await;
    assert!(
        native.requests.lock().unwrap().last().unwrap()["messages"]
            .to_string()
            .contains("Native summary.")
    );
    app.close().await.unwrap();
}

#[tokio::test]
async fn anthropic_custom_models_respect_the_image_setting() {
    let native = Native::start().await;
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("fixture.txt"), "image fixture").unwrap();
    let mut settings = settings(root.path(), state.path(), &native.url);
    settings.provider = ProviderKind::Anthropic;
    settings.model = "custom-gateway-model".into();
    let mut app = Application::open(&settings, "native-fixture-key".into())
        .await
        .unwrap();
    assert!(!app.view.can_attach_images);
    edit_profile(&mut app, "agents", "edit:code", &[("vision", "enabled")]).await;
    assert!(app.view.can_attach_images);
    let image = crate::images::tests::fixture().unwrap();
    let (client, task) = super::images::attach(&app, root.path()).await;
    client
        .try_send(super::images::request(
            &app,
            Command::SubmitImages {
                agent: "code".into(),
                message: Box::new(roomkit_tui::protocol::ImageMessage::new(
                    "Describe this image.",
                    vec![image.clone()],
                )),
            },
        ))
        .unwrap();
    super::images::completed(&client).await;
    let has_image = |request: &Value| {
        request["messages"].as_array().unwrap().iter().any(|m| {
            m["content"].as_array().is_some_and(|parts| {
                parts
                    .iter()
                    .any(|p| p["type"] == "image" && p["source"]["data"] == image.data)
            })
        })
    };
    assert!(has_image(&native.requests.lock().unwrap()[0]));
    drop(client);
    task.await.unwrap().unwrap();
    edit_profile(&mut app, "agents", "edit:code", &[("vision", "disabled")]).await;
    turn_operation(&app, "images-disabled").await;
    assert!(!has_image(native.requests.lock().unwrap().last().unwrap()));
    app.close().await.unwrap();
}
