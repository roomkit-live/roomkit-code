//! RMK-1532: an agent hands a task to another configured agent with the
//! engine's `delegate_task`. The worker runs in a room of its own, in the
//! background, under the conversation's mode, refused what needs an approval;
//! the delegating agent then says its result in a turn of its own.

use super::*;

mod tasks;

/// What tells the worker's requests apart: the line its turns read.
const WORKER: &str = "nobody watches";
/// The worker's answer, which the hand-back carries to the delegating agent.
const RESULT: &str = "WORKER_RESULT_THE_DIFF_IS_FINE";
/// What the delegating agent says once it is handed the result.
const RELAYED: &str = "RELAYED_THE_REVIEW";

/// The task `code` hands `target`, with a context.
fn task_for(target: &str) -> Value {
    json!({"agent": target, "task": "Review the diff", "context": {"branch": "BRANCH_MARK"}})
}

/// A model server whose answer depends on who asks and what it was told
/// last: the worker writes a file, then answers; the delegating agent calls
/// `delegate_task` with `arguments`, then says it did, then relays the result
/// it is handed.
async fn server(arguments: Value) -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(request): Json<Value>| {
            let (seen, arguments) = (seen.clone(), arguments.clone());
            async move {
                seen.lock().unwrap().push(request.clone());
                let worker = request.to_string().contains(WORKER);
                let last = request["messages"].as_array().unwrap().last().unwrap();
                let answered = last["role"] == "tool";
                let delta = if !worker && last.to_string().contains(RESULT) {
                    json!({"content": RELAYED})
                } else if worker && !answered {
                    let args = json!({"path": "worker.txt", "content": "from the worker"});
                    call("write", &args)
                } else if worker {
                    json!({"content": RESULT})
                } else if !answered {
                    call("delegate_task", &arguments)
                } else {
                    json!({"content": "Handed to the reviewer."})
                };
                let reason = if delta.get("tool_calls").is_some() {
                    "tool_calls"
                } else {
                    "stop"
                };
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

/// Whether `request` is the delegating agent's turn on a result handed back:
/// its last message carries the worker's answer, which earlier turns of the
/// conversation may carry too.
fn hands_back(request: &Value) -> bool {
    let last = request["messages"].as_array().unwrap().last().unwrap();
    !request.to_string().contains(WORKER) && last.to_string().contains(RESULT)
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

/// Waits until the delegating agent was handed a result: the hand-back opens
/// a turn of its own once the worker is done.
async fn until_handed_back(requests: &Arc<Mutex<Vec<Value>>>) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !requests.lock().unwrap().iter().any(hands_back) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

/// Waits until the delegating agent said the result in the conversation and
/// its turn ended: the view is busy while it runs.
async fn until_relayed(app: &Application) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
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
}

async fn delegated(project: &std::path::Path, mode: &str) -> Delegated {
    let state = tempfile::tempdir().unwrap();
    let (base, requests, server) = server(task_for("review")).await;
    let settings = settings(project, state.path(), &base);
    with_a_reviewer(&settings).await;
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(app.view.agent.as_str(), "code");
    app.policy.set("main", mode).unwrap();

    turn(&app).await;
    until_handed_back(&requests).await;
    until_relayed(&app).await;
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

/// What the gate answered a `delegate_task` call with `arguments`, which it
/// refused: no worker ran.
async fn refusal(arguments: Value) -> String {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (base, requests, server) = server(arguments).await;
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
    let requests = requests.lock().unwrap();
    assert!(!requests.iter().any(|r| r.to_string().contains(WORKER)));
    refused.text
}

/// A task handed to no other configured agent is refused, naming those it
/// can be handed to.
#[tokio::test]
async fn a_task_for_an_unknown_agent_is_refused() {
    let refused = refusal(task_for("terminal")).await;
    assert!(
        refused.contains("Hand a task only to one of these agents: review."),
        "{refused}"
    );
}

/// A model that shares a channel with the worker anyway, the terminal's here,
/// is refused: the terminal would be delivered the worker's answers.
#[tokio::test]
async fn a_task_sharing_a_channel_is_refused() {
    let mut arguments = task_for("review");
    arguments["share_channels"] = json!(["terminal"]);
    let refused = refusal(arguments).await;
    assert!(
        refused.contains("share_channels is not available"),
        "{refused}"
    );
}

/// A task handed to an agent set up by an earlier task reaches the agent as
/// its connection stands now: a connection edited since reinstalls only the
/// selected agent.
#[tokio::test]
async fn a_target_works_with_its_connection_as_it_stands() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (first, before, first_server) = server(task_for("review")).await;
    let (second, after, second_server) = server(task_for("review")).await;
    let settings = settings(project.path(), state.path(), &first);
    with_a_reviewer(&settings).await;
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    turn(&app).await;
    until_handed_back(&before).await;
    until_relayed(&app).await;

    let provider = saved_provider_id(&settings);
    edit_profile(
        &mut app,
        "providers",
        &format!("edit:{provider}"),
        &[("url", second.as_str())],
    )
    .await;
    turn_operation(&app, "second-task").await;
    until_handed_back(&after).await;
    app.close().await.unwrap();
    first_server.abort();
    second_server.abort();

    let worker = |requests: &Arc<Mutex<Vec<Value>>>| {
        let requests = requests.lock().unwrap();
        requests
            .iter()
            .filter(|r| r.to_string().contains(WORKER))
            .count()
    };
    assert_eq!((worker(&before), worker(&after)), (2, 2));
}

/// A model server whose workers each hand the task on to the other agent
/// and answer once that is done, and whose delegating agent starts the chain:
/// `code`, then `review`, `code` and `review` again, which is refused.
async fn nesting_server() -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(request): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(request.clone());
                let text = request.to_string();
                let worker = text.contains(WORKER);
                let last = request["messages"].as_array().unwrap().last().unwrap();
                let (answered, handed_back) =
                    (last["role"] == "tool", last.to_string().contains(RESULT));
                let other = if text.contains("(ID: review)") {
                    "code"
                } else {
                    "review"
                };
                let delta = if handed_back || (worker && answered) {
                    json!({"content": RESULT})
                } else if worker || !answered {
                    call(
                        "delegate_task",
                        &json!({"agent": other, "task": "Go one level deeper"}),
                    )
                } else {
                    json!({"content": "Handed on."})
                };
                let reason = if delta.get("tool_calls").is_some() {
                    "tool_calls"
                } else {
                    "stop"
                };
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

/// Two agents handing a task back and forth stop three delegated rooms deep:
/// the third worker's `delegate_task` is refused, so a model that keeps
/// handing work on spends no more.
#[tokio::test]
async fn a_task_is_handed_on_three_levels_deep_at_most() {
    const REFUSED: &str = "handed on too many times";
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (base, requests, server) = nesting_server().await;
    let settings = settings(project.path(), state.path(), &base);
    with_a_reviewer(&settings).await;
    let app = Application::open(&settings, String::new()).await.unwrap();
    turn(&app).await;
    let refused = |requests: &Arc<Mutex<Vec<Value>>>| {
        let requests = requests.lock().unwrap();
        requests
            .iter()
            .filter(|r| r.to_string().contains(REFUSED))
            .count()
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        while refused(&requests) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    app.close().await.unwrap();
    server.abort();

    let requests = requests.lock().unwrap();
    let delegated = requests
        .iter()
        .filter(|r| {
            let last = r["messages"].as_array().unwrap().last().unwrap();
            last["role"] == "tool" && last.to_string().contains("delegated")
        })
        .count();
    // The conversation's agent, its worker and that worker's worker each
    // handed the task on; the next one was refused.
    assert_eq!(delegated, 3);
    let deepest = requests
        .iter()
        .find(|r| r.to_string().contains(REFUSED))
        .unwrap();
    assert!(deepest.to_string().contains("(ID: review)") && deepest.to_string().contains(WORKER));
}

/// A launch nobody watches ends with its turn, before a task handed over
/// could end: its agents are neither served the delegation tools nor told of
/// other agents.
#[tokio::test]
async fn an_unattended_launch_offers_no_delegation() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let mut settings = settings(project.path(), state.path(), &mock.base_url);
    with_a_reviewer(&settings).await;
    settings.unattended = true;
    let app = Application::open(&settings, String::new()).await.unwrap();
    turn(&app).await;
    app.close().await.unwrap();

    let request = mock.requests.lock().unwrap()[0].clone();
    let tools: Vec<&str> = request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect();
    let text = request.to_string();
    for name in ["delegate_task", "task_status", "cancel_task"] {
        assert!(!tools.contains(&name), "{name}");
    }
    assert!(!text.contains("hand a task to"));
}
