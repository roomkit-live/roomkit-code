//! RMK-1532: an agent hands a task to another configured agent with the
//! engine's `delegate_task`. The worker runs in a room of its own, in the
//! background, under the conversation's mode, refused what needs an approval;
//! the delegating agent then says its result in a turn of its own.

use super::*;

/// What tells the worker's requests apart: the line its turns read.
const WORKER: &str = "nobody watches";
/// The worker's answer, which the hand-back carries to the delegating agent.
const RESULT: &str = "WORKER_RESULT_THE_DIFF_IS_FINE";
/// What the delegating agent says once it is handed the result.
const RELAYED: &str = "RELAYED_THE_REVIEW";

/// A model server whose answer depends on who asks: the worker writes a file,
/// then answers; the delegating agent hands the task over, then says it did,
/// then relays the result it is handed. `target` is the agent the task is
/// handed to.
async fn server(
    target: &'static str,
) -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(request): Json<Value>| {
            let seen = seen.clone();
            async move {
                let text = request.to_string();
                seen.lock().unwrap().push(request.clone());
                let answered = request["messages"]
                    .as_array()
                    .is_some_and(|messages| messages.iter().any(|m| m["role"] == "tool"));
                let delta = if text.contains(RESULT) && !text.contains(WORKER) {
                    json!({"content": RELAYED})
                } else if text.contains(WORKER) && !answered {
                    let args = json!({"path": "worker.txt", "content": "from the worker"});
                    call("write", &args)
                } else if text.contains(WORKER) {
                    json!({"content": RESULT})
                } else if !answered {
                    let args = json!({"agent": target, "task": "Review the diff", "context": {"branch": "BRANCH_MARK"}});
                    call("delegate_task", &args)
                } else {
                    json!({"content": "Handed to the reviewer."})
                };
                let reason = if delta.get("tool_calls").is_some() { "tool_calls" } else { "stop" };
                let body = format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"choices":[{"index":0,"delta":delta}]}),
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]})
                );
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (base, requests, task)
}

fn call(name: &str, arguments: &Value) -> Value {
    json!({"tool_calls":[{"index":0,"id":format!("call-{name}"),"type":"function","function":{"name":name,"arguments":arguments.to_string()}}]})
}

/// The project with a second agent, `review` labelled "Review", a copy of
/// `code` never selected.
async fn with_a_reviewer(settings: &Settings) {
    let app = Application::open(settings, String::new()).await.unwrap();
    let path = app._storage.database.with_file_name("agents.json");
    app.close().await.unwrap();
    drop(app);
    let mut document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut review = document["agents"]["code"].clone();
    review["id"] = json!("review");
    review["label"] = json!("Review");
    document["agents"]["review"] = review;
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
}

/// Waits until `requests` hold one that satisfies `found`.
async fn until_asked(requests: &Arc<Mutex<Vec<Value>>>, found: impl Fn(&str) -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| found(&r.to_string()))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

/// What a conversation in `mode` showed once `code` handed `review` a task
/// in `project` and said its result: every request the model server got,
/// and the conversation's status line then.
struct Delegated {
    requests: Vec<Value>,
    notices: Vec<String>,
}

impl Delegated {
    /// The worker's requests, as text.
    fn worker(&self) -> Vec<String> {
        self.requests
            .iter()
            .map(Value::to_string)
            .filter(|r| r.contains(WORKER))
            .collect()
    }
}

async fn delegated(project: &std::path::Path, mode: &str) -> Delegated {
    let state = tempfile::tempdir().unwrap();
    let (base, requests, server) = server("review").await;
    let settings = settings(project, state.path(), &base);
    with_a_reviewer(&settings).await;
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(app.view.agent.as_str(), "code");
    app.policy.set("main", mode).unwrap();

    turn(&app).await;
    // The hand-back opens a turn of the delegating agent once the worker is done.
    until_asked(&requests, |r| r.contains(RESULT) && !r.contains(WORKER)).await;
    // The delegating agent says the result in the conversation.
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            // The view is busy while the hand-back's turn runs.
            if let Ok(snapshot) = app.session().snapshot().await
                && snapshot
                    .items
                    .iter()
                    .any(|item| matches!(item, Item::Message(m) if m.text == RELAYED))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let notices = app.profiles.resources.tasks.notices("main");
    app.close().await.unwrap();
    server.abort();
    let requests = requests.lock().unwrap().clone();
    Delegated { requests, notices }
}

#[tokio::test]
async fn an_agent_hands_a_task_to_another_and_says_its_result() {
    let project = tempfile::tempdir().unwrap();
    let delegated = delegated(project.path(), "ask").await;

    // The delegating agent is served the three tools, sharing no channel, and
    // reads the agents it can hand a task to.
    let first = &delegated.requests[0];
    let tools: Vec<&str> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect();
    for name in ["delegate_task", "task_status", "cancel_task"] {
        assert!(tools.contains(&name), "{name}: {tools:?}");
    }
    let delegate = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["function"]["name"] == "delegate_task")
        .unwrap();
    assert!(delegate["function"]["parameters"]["properties"]["share_channels"].is_null());
    assert!(first.to_string().contains("review (Review)"));

    // The worker read the task and its context, under Ask, and none of the
    // conversation; its write needed an approval and was refused.
    let worker = delegated.worker();
    assert_eq!(worker.len(), 2, "{worker:?}");
    assert!(worker[0].contains("Review the diff") && worker[0].contains("BRANCH_MARK"));
    assert!(worker[0].contains("Application mode: Ask"));
    assert!(!worker[0].contains("Inspect the project."));
    assert!(
        worker[1].contains("needs the person's approval"),
        "{}",
        worker[1]
    );
    assert!(!project.path().join("worker.txt").exists());

    // The status line said how the task ended.
    let notices = &delegated.notices;
    assert!(
        notices
            .iter()
            .any(|n| n.starts_with("Task for Review completed")),
        "{notices:?}"
    );
}

/// The worker takes its conversation's mode, not the one the application was
/// launched in (Ask here): in a Yolo conversation its write runs.
#[tokio::test]
async fn a_worker_takes_its_conversation_s_mode() {
    let project = tempfile::tempdir().unwrap();
    let delegated = delegated(project.path(), "yolo").await;

    let worker = delegated.worker();
    assert_eq!(worker.len(), 2, "{worker:?}");
    assert!(worker[0].contains("Application mode: Yolo"));
    assert!(!worker[1].contains("needs the person's approval"));
    let written = std::fs::read_to_string(project.path().join("worker.txt")).unwrap();
    assert_eq!(written, "from the worker");
}

/// A task handed to no other configured agent is refused, naming those it
/// can be handed to, and no worker runs.
#[tokio::test]
async fn a_task_for_an_unknown_agent_is_refused() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (base, requests, server) = server("terminal").await;
    let settings = settings(project.path(), state.path(), &base);
    with_a_reviewer(&settings).await;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let session = turn(&app).await.snapshot().cloned().unwrap();
    app.close().await.unwrap();
    server.abort();

    let refused = session
        .items
        .iter()
        .find_map(|item| match item {
            Item::Tool { call, .. } if call.name == "delegate_task" => Some(call.clone()),
            _ => None,
        })
        .unwrap();
    assert_ne!(refused.state, ToolState::Succeeded);
    assert!(
        refused
            .text
            .contains("Hand a task only to one of these agents: review."),
        "{}",
        refused.text
    );
    let requests = requests.lock().unwrap();
    assert!(!requests.iter().any(|r| r.to_string().contains(WORKER)));
}
