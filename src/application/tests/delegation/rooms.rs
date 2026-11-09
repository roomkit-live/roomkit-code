//! RMK-1535: a view moves into a delegated task's room, follows its worker,
//! writes to it while it works and after it finished, and comes back.

use std::sync::atomic::{AtomicUsize, Ordering};

use roomkit_tui::Client;

use super::*;

/// What the person writes to the worker while it works.
const STEERED: &str = "FROM_THE_PERSON_WHILE_IT_WORKS";
/// What the person writes to the worker once its task ended.
const LATER: &str = "FROM_THE_PERSON_AFTER_THE_TASK";
/// What the worker says to that.
const LATER_ANSWER: &str = "WORKER_LATER_ANSWER";

/// A model server whose worker keeps listing the project until the person
/// wrote to it, then gives its result; the delegating agent hands `review`
/// the task, says it did, then relays the result.
async fn attentive_worker() -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let rounds = Arc::new(AtomicUsize::new(0));
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(request): Json<Value>| {
            let (seen, rounds) = (seen.clone(), rounds.clone());
            async move {
                seen.lock().unwrap().push(request.clone());
                let text = request.to_string();
                let worker = text.contains(WORKER);
                let last = request["messages"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()
                    .clone();
                let delta = if worker && text.contains(LATER) {
                    json!({"content": LATER_ANSWER})
                } else if worker && text.contains(STEERED) {
                    json!({"content": RESULT})
                } else if worker {
                    let round = rounds.fetch_add(1, Ordering::SeqCst);
                    assert!(round < 400, "The person's message never reached the worker");
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    call("list", &json!({"path": format!("./{round}")}))
                } else if last.to_string().contains(RESULT) {
                    json!({"content": RELAYED})
                } else if last["role"] == "tool" {
                    json!({"content": "Handed to the reviewer."})
                } else {
                    call("delegate_task", &task_for("review"))
                };
                let reason = if delta.get("tool_calls").is_some() {
                    "tool_calls"
                } else {
                    "stop"
                };
                let body = format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"choices":[{"index":0,"delta":delta}]}),
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]}),
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

/// The view's events until `found` returns something for one.
async fn until<T>(client: &Client, mut found: impl FnMut(&Event) -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match client.try_recv() {
                Ok(event) => {
                    if let Some(value) = found(&event) {
                        return value;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                Err(error) => panic!("The view ended: {error}"),
            }
        }
    })
    .await
    .unwrap()
}

/// The next snapshot of `room`, once the switch was accepted.
async fn snapshot_of(client: &Client, room: &str) -> roomkit_tui::protocol::Snapshot {
    until(client, |event| match event {
        Event::Snapshot(snapshot) if snapshot.conversation.as_str() == room => {
            Some(snapshot.clone())
        }
        Event::Reply {
            error: Some(error), ..
        } => panic!("Refused: {error:?}"),
        _ => None,
    })
    .await
}

fn invoke(id: u64, room: &str, name: &str, arguments: &str) -> Request {
    let command = Command::Invoke {
        name: name.into(),
        arguments: arguments.into(),
    };
    Request::new(RequestId::new(id), format!("op-{id}"), room, command)
}

fn submit(id: u64, room: &str, agent: &str, text: &str) -> Request {
    let command = Command::Submit {
        agent: agent.into(),
        text: text.into(),
    };
    Request::new(RequestId::new(id), format!("op-{id}"), room, command)
}

async fn until_requested(requests: &Arc<Mutex<Vec<Value>>>, found: impl Fn(&Value) -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !requests.lock().unwrap().iter().any(&found) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_view_moves_into_a_task_s_room_writes_to_its_worker_and_comes_back() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (base, requests, server) = attentive_worker().await;
    let settings = settings(project.path(), state.path(), &base);
    with_a_reviewer(&settings).await;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, host) = connection(NonZeroUsize::new(1_024).unwrap());
    let router = app.router(&settings.workspace);
    let view = tokio::spawn(app.session().run_with_commands(host, router));
    let first = snapshot_of(&client, "main").await;
    assert_eq!(first.cycle_command.as_deref(), Some("tasks"));

    client
        .try_send(submit(1, "main", "code", "Review the diff"))
        .unwrap();
    until_requested(&requests, |request| request.to_string().contains(WORKER)).await;
    let tasks = app.profiles.resources.tasks.clone();
    let task = tasks.of("main").remove(0).1;

    // Alt+Right: the worker's room, with its agent, the run under way.
    client.try_send(invoke(2, "main", "tasks", "next")).unwrap();
    let opened = snapshot_of(&client, &task.room).await;
    assert_eq!(opened.selected_agent.as_str(), "review");
    assert!(
        opened.title.starts_with("Task for Review"),
        "{}",
        opened.title
    );
    until(&client, |event| {
        matches!(event, Event::RunStarted { .. }).then_some(())
    })
    .await;

    // Written to while it works, it reads the message at its next step, and
    // the view follows it to its answer.
    client
        .try_send(submit(3, &task.room, "review", STEERED))
        .unwrap();
    until(&client, |event| match event {
        Event::MessageFinalized { message, .. } if message.text == RESULT => Some(()),
        Event::Reply {
            error: Some(error), ..
        } => panic!("Refused: {error:?}"),
        _ => None,
    })
    .await;

    // Alt+Left: back in the conversation, the worker's result was relayed.
    client
        .try_send(invoke(4, &task.room, "tasks", "previous"))
        .unwrap();
    snapshot_of(&client, "main").await;
    until_handed_back(&requests).await;

    // The task's room is no session of its own.
    let elsewhere = crate::sessions::commands();
    assert!(elsewhere.iter().any(|spec| spec.name == "resume"));
    client.try_send(invoke(5, "main", "resume", "")).unwrap();
    until(&client, |event| match event {
        Event::Choices { choices, .. } => {
            assert!(!format!("{choices:?}").contains("task-"), "{choices:?}");
            Some(())
        }
        Event::Reply {
            request,
            error: Some(_),
        } if *request == RequestId::new(5) => Some(()),
        _ => None,
    })
    .await;

    // Its task ended: /tasks opens its room, and what is said there stays
    // there: the delegating agent is asked nothing more.
    until_relayed(&app).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    while client.try_recv().is_ok() {}
    client
        .try_send(invoke(6, "main", "tasks", &task.id))
        .unwrap();
    let panel = until(&client, |event| match event {
        Event::Panel { panel, .. } => Some(panel.clone()),
        _ => None,
    })
    .await;
    let room = format!("{}:room:{}", panel.id, task.id);
    assert!(
        panel.actions.iter().any(|choice| choice.value == room),
        "{:?}",
        panel.actions
    );
    client.try_send(invoke(7, "main", "tasks", &room)).unwrap();
    snapshot_of(&client, &task.room).await;
    let delegating = |requests: &Arc<Mutex<Vec<Value>>>| {
        let requests = requests.lock().unwrap();
        requests
            .iter()
            .filter(|r| !r.to_string().contains(WORKER))
            .count()
    };
    let before = delegating(&requests);
    client
        .try_send(submit(8, &task.room, "review", LATER))
        .unwrap();
    until(&client, |event| match event {
        Event::MessageFinalized { message, .. } if message.text == LATER_ANSWER => Some(()),
        Event::Reply {
            error: Some(error), ..
        } => panic!("Refused: {error:?}"),
        _ => None,
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        delegating(&requests),
        before,
        "The delegating agent was told"
    );
    assert!(!tasks.of("main").remove(0).1.running());

    drop(client);
    view.await.unwrap().unwrap();
    app.close().await.unwrap();
    server.abort();
}

/// From the task's room, the key that stops a run stops its task, which the
/// view did not start: the delegating agent is told it was cancelled.
#[tokio::test]
async fn the_view_of_a_running_task_stops_it() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (base, requests, server) = attentive_worker().await;
    let settings = settings(project.path(), state.path(), &base);
    with_a_reviewer(&settings).await;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, host) = connection(NonZeroUsize::new(1_024).unwrap());
    let router = app.router(&settings.workspace);
    let view = tokio::spawn(app.session().run_with_commands(host, router));
    snapshot_of(&client, "main").await;
    client
        .try_send(submit(1, "main", "code", "Review the diff"))
        .unwrap();
    until_requested(&requests, |request| request.to_string().contains(WORKER)).await;
    let tasks = app.profiles.resources.tasks.clone();
    let task = tasks.of("main").remove(0).1;
    client.try_send(invoke(2, "main", "tasks", "next")).unwrap();
    snapshot_of(&client, &task.room).await;
    let run = until(&client, |event| match event {
        Event::RunStarted { run, .. } => Some(run.clone()),
        _ => None,
    })
    .await;

    let stop = Command::Interrupt { run };
    client
        .try_send(Request::new(
            RequestId::new(9),
            "stop",
            task.room.as_str(),
            stop,
        ))
        .unwrap();
    until(&client, |event| match event {
        Event::Reply { request, error } if *request == RequestId::new(9) => {
            assert_eq!(*error, None);
            Some(())
        }
        _ => None,
    })
    .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while tasks.running() > 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        tasks
            .of("main")
            .remove(0)
            .1
            .state()
            .starts_with("cancelled")
    );

    drop(client);
    view.await.unwrap().unwrap();
    app.close().await.unwrap();
    server.abort();
}

/// A worker that hands its task on gets the result back: its room holds a
/// transport to deliver it, now that the terminal is shared there.
#[tokio::test]
async fn a_task_handed_on_comes_back_to_the_worker_that_handed_it() {
    const NESTED: &str = "NESTED_WORKER_RESULT";
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(request): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(request.clone());
                let text = request.to_string();
                let last = request["messages"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()
                    .clone();
                let handing = json!({"agent": "code", "task": "Check it", "context": {}});
                // The third agent reads only the task handed on to it.
                let third = text.contains(WORKER)
                    && text.contains("Check it")
                    && !text.contains("Review the diff");
                let delta = if third {
                    json!({"content": NESTED})
                } else if last.to_string().contains(NESTED) {
                    json!({"content": RESULT})
                } else if text.contains(WORKER) && last["role"] == "tool" {
                    json!({"content": "Handed on."})
                } else if text.contains(WORKER) {
                    call("delegate_task", &handing)
                } else if last.to_string().contains(RESULT) {
                    json!({"content": RELAYED})
                } else if last["role"] == "tool" {
                    json!({"content": "Handed to the reviewer."})
                } else {
                    call("delegate_task", &task_for("review"))
                };
                let reason = if delta.get("tool_calls").is_some() {
                    "tool_calls"
                } else {
                    "stop"
                };
                let body = format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"choices":[{"index":0,"delta":delta}]}),
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]}),
                );
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let settings = settings(project.path(), state.path(), &base);
    with_a_reviewer(&settings).await;
    let app = Application::open(&settings, String::new()).await.unwrap();
    turn(&app).await;
    until_requested(&requests, |request| {
        let last = request["messages"].as_array().unwrap().last().unwrap();
        request.to_string().contains(WORKER) && last.to_string().contains(NESTED)
    })
    .await;
    app.close().await.unwrap();
    server.abort();
}
