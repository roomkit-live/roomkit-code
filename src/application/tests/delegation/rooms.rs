//! RMK-1535: a view moves into a delegated task's room, follows its worker,
//! writes to it while it works and after it finished, and comes back.

use std::sync::atomic::{AtomicUsize, Ordering};

use roomkit_tui::Client;
use roomkit_tui::protocol::Decision;

use super::*;

/// What the person writes to the worker while it works.
const STEERED: &str = "FROM_THE_PERSON_WHILE_IT_WORKS";
/// What the person writes to the worker once its task ended.
const LATER: &str = "FROM_THE_PERSON_AFTER_THE_TASK";
/// What the worker says to that.
const LATER_ANSWER: &str = "WORKER_LATER_ANSWER";

/// Whether a request is a worker's: while its task runs, or once it ended.
pub(super) fn is_worker(text: &str) -> bool {
    text.contains(WORKER) || text.contains(crate::delegation::AFTER_TASK)
}

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
                let worker = is_worker(&text);
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
    assert!(
        !opened.can_cut_to_send,
        "Cutting its run would stop the task"
    );

    until(&client, |event| {
        matches!(event, Event::RunStarted { .. }).then_some(())
    })
    .await;

    // A compaction would start at once: nothing queues it behind a task.
    client
        .try_send(invoke(20, &task.room, "compact", ""))
        .unwrap();
    until(&client, |event| match event {
        Event::Reply { request, error } if *request == RequestId::new(20) => {
            assert!(error.is_some(), "A running task's room was compacted");
            Some(())
        }
        _ => None,
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
    let home = snapshot_of(&client, "main").await;
    assert!(
        home.can_cut_to_send,
        "The conversation kept the task's rule"
    );
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

/// Runs a task through to its result, written to while it works, then leaves
/// the view in the conversation with the result relayed.
async fn a_finished_task(
    app: &Application,
    client: &Client,
    requests: &Arc<Mutex<Vec<Value>>>,
) -> crate::delegation::Task {
    snapshot_of(client, "main").await;
    client
        .try_send(submit(1, "main", "code", "Review the diff"))
        .unwrap();
    until_requested(requests, |request| request.to_string().contains(WORKER)).await;
    let task = app.profiles.resources.tasks.of("main").remove(0).1;
    client.try_send(invoke(2, "main", "tasks", "next")).unwrap();
    snapshot_of(client, &task.room).await;
    client
        .try_send(submit(3, &task.room, "review", STEERED))
        .unwrap();
    until(client, |event| match event {
        Event::MessageFinalized { message, .. } if message.text == RESULT => Some(()),
        _ => None,
    })
    .await;
    client
        .try_send(invoke(4, &task.room, "tasks", "previous"))
        .unwrap();
    snapshot_of(client, "main").await;
    until_handed_back(requests).await;
    until_relayed(app).await;
    task
}

/// Opens `task`'s room through `/tasks`.
async fn open_room(client: &Client, task: &crate::delegation::Task) {
    client
        .try_send(invoke(30, "main", "tasks", &task.id))
        .unwrap();
    let panel = until(client, |event| match event {
        Event::Panel { panel, .. } => Some(panel.clone()),
        Event::Reply {
            error: Some(error), ..
        } => panic!("Refused: {error:?}"),
        _ => None,
    })
    .await;
    let room = format!("{}:room:{}", panel.id, task.id);
    client.try_send(invoke(31, "main", "tasks", &room)).unwrap();
    snapshot_of(client, &task.room).await;
}

/// After a restart the worker was never assembled: opening its room readies
/// it, and it answers.
#[tokio::test]
async fn a_task_of_an_earlier_launch_can_be_written_to() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (base, requests, server) = attentive_worker().await;
    let settings = settings(project.path(), state.path(), &base);
    with_a_reviewer(&settings).await;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, host) = connection(NonZeroUsize::new(1_024).unwrap());
    let router = app.router(&settings.workspace);
    let view = tokio::spawn(app.session().run_with_commands(host, router));
    let task = a_finished_task(&app, &client, &requests).await;
    drop(client);
    view.await.unwrap().unwrap();
    app.close().await.unwrap();
    drop(app);

    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, host) = connection(NonZeroUsize::new(1_024).unwrap());
    let router = app.router(&settings.workspace);
    let view = tokio::spawn(app.session().run_with_commands(host, router));
    snapshot_of(&client, "main").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.profiles.resources.tasks.of("main").is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let task = app.profiles.resources.tasks.find("main", &task.id).unwrap();
    open_room(&client, &task).await;
    client
        .try_send(submit(32, &task.room, "review", LATER))
        .unwrap();
    until(&client, |event| match event {
        Event::MessageFinalized { message, .. } if message.text == LATER_ANSWER => Some(()),
        Event::Reply {
            error: Some(error), ..
        } => panic!("Refused: {error:?}"),
        _ => None,
    })
    .await;
    drop(client);
    view.await.unwrap().unwrap();
    app.close().await.unwrap();
    server.abort();
}

/// A later task for the same agent does not take the loop the first one runs
/// in: a message to the first still reaches it.
#[tokio::test]
async fn a_second_task_for_the_agent_leaves_the_first_within_reach() {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let rounds = Arc::new(AtomicUsize::new(0));
    let seen_rounds = rounds.clone();
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(request): Json<Value>| {
            let (seen, rounds) = (seen.clone(), rounds.clone());
            async move {
                seen.lock().unwrap().push(request.clone());
                let text = request.to_string();
                let last = request["messages"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()
                    .clone();
                let second = json!({"agent": "review", "task": "Second task"});
                let handed = request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|message| message["role"] == "tool")
                    .count();
                let delta = if is_worker(&text) && text.contains("Second task") {
                    json!({"content": "SECOND_DONE"})
                } else if is_worker(&text) && text.contains(STEERED) {
                    json!({"content": RESULT})
                } else if is_worker(&text) {
                    let round = rounds.fetch_add(1, Ordering::SeqCst);
                    assert!(round < 600, "The person's message never reached the worker");
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    call("list", &json!({"path": format!("./{round}")}))
                } else if last.to_string().contains("SECOND_DONE") {
                    json!({"content": "Noted."})
                } else if last.to_string().contains(RESULT) {
                    json!({"content": RELAYED})
                } else if handed == 0 {
                    call("delegate_task", &task_for("review"))
                } else if handed == 1 {
                    // Once the first task's worker is at work: assembled
                    // before it began, it would simply run on the new one.
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    call("delegate_task", &second)
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
    let (client, host) = connection(NonZeroUsize::new(4_096).unwrap());
    let router = app.router(&settings.workspace);
    let view = tokio::spawn(app.session().run_with_commands(host, router));
    snapshot_of(&client, "main").await;
    client
        .try_send(submit(1, "main", "code", "Review the diff"))
        .unwrap();
    // The second task was handed over, once the first's worker runs.
    until_requested(&requests, |request| {
        request.to_string().contains("Second task") && is_worker(&request.to_string())
    })
    .await;
    let tasks = app.profiles.resources.tasks.clone();
    let first = tasks.of("main").remove(0).1;
    assert!(first.running());
    client.try_send(invoke(2, "main", "tasks", "next")).unwrap();
    snapshot_of(&client, &first.room).await;
    let at_writing = seen_rounds.load(Ordering::SeqCst);
    client
        .try_send(submit(3, &first.room, "review", STEERED))
        .unwrap();
    until(&client, |event| match event {
        Event::MessageFinalized { message, .. } if message.text == RESULT => Some(()),
        Event::Reply {
            error: Some(error), ..
        } => panic!("Refused: {error:?}"),
        _ => None,
    })
    .await;
    // At its next step, not once the agent's turn ran out of rounds.
    let rounds_after = seen_rounds.load(Ordering::SeqCst) - at_writing;
    assert!(rounds_after < 10, "It took {rounds_after} more rounds");
    drop(client);
    view.await.unwrap().unwrap();
    app.close().await.unwrap();
    server.abort();
}

/// What a person's turn in an ended task's room consumes reaches the total.
#[tokio::test]
async fn a_turn_in_an_ended_task_s_room_is_counted() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (base, requests, server) = super::spending::reporting_server(false).await;
    let settings = super::spending::priced_settings(project.path(), state.path(), &base).await;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, host) = connection(NonZeroUsize::new(1_024).unwrap());
    let router = app.router(&settings.workspace);
    let view = tokio::spawn(app.session().run_with_commands(host, router));
    snapshot_of(&client, "main").await;
    client
        .try_send(submit(1, "main", "code", "Review the diff"))
        .unwrap();
    until_handed_back(&requests).await;
    until_relayed(&app).await;
    let total = |micros: u64| {
        move |event: &Event| match event {
            Event::RelatedCostUpdated {
                cost: Some(cost), ..
            } if cost.micros == micros => Some(()),
            _ => None,
        }
    };
    until(&client, total(3_000)).await;
    let task = app.profiles.resources.tasks.of("main").remove(0).1;
    open_room(&client, &task).await;
    client
        .try_send(submit(40, &task.room, "review", "One more thing"))
        .unwrap();
    until(&client, |event| match event {
        Event::RunFinished { .. } => Some(()),
        Event::Reply {
            error: Some(error), ..
        } => panic!("Refused: {error:?}"),
        _ => None,
    })
    .await;
    client
        .try_send(invoke(41, &task.room, "tasks", "previous"))
        .unwrap();
    snapshot_of(&client, "main").await;
    until(&client, total(6_000)).await;
    drop(client);
    view.await.unwrap().unwrap();
    app.close().await.unwrap();
    server.abort();
}

/// A model server whose worker, once its task ended, answers the person with
/// a tool call and then never in time: a turn the person cuts on no text.
async fn worker_cut_after_a_tool() -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>)
{
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(request): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(request.clone());
                let text = request.to_string();
                let worker = is_worker(&text);
                let last = request["messages"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()
                    .clone();
                let later = worker && text.contains("One more thing");
                if later && last["role"] == "tool" {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
                let delta = if later {
                    call("list", &json!({"path": "."}))
                } else if worker {
                    json!({"content": RESULT})
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
                let usage = if worker {
                    json!({"prompt_tokens": 1_000, "completion_tokens": 100})
                } else {
                    json!({"prompt_tokens": 2_000, "completion_tokens": 200})
                };
                let body = format!(
                    "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"choices":[{"index":0,"delta":delta}]}),
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]}),
                    json!({"choices":[], "usage": usage}),
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

/// A turn cut on no text records itself by updating a row: the total still
/// learns of it, and that it is not whole.
#[tokio::test]
async fn a_turn_cut_on_no_text_in_an_ended_task_s_room_is_counted() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (base, requests, server) = worker_cut_after_a_tool().await;
    let settings = super::spending::priced_settings(project.path(), state.path(), &base).await;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, host) = connection(NonZeroUsize::new(1_024).unwrap());
    let router = app.router(&settings.workspace);
    let view = tokio::spawn(app.session().run_with_commands(host, router));
    snapshot_of(&client, "main").await;
    client
        .try_send(submit(1, "main", "code", "Review the diff"))
        .unwrap();
    until_handed_back(&requests).await;
    until_relayed(&app).await;
    let task = app.profiles.resources.tasks.of("main").remove(0).1;
    open_room(&client, &task).await;
    client
        .try_send(submit(40, &task.room, "review", "One more thing"))
        .unwrap();
    let run = until(&client, |event| match event {
        Event::RunStarted { run, .. } => Some(run.clone()),
        _ => None,
    })
    .await;
    until_requested(&requests, |request| {
        let last = request["messages"].as_array().unwrap().last().unwrap();
        request.to_string().contains("One more thing") && last["role"] == "tool"
    })
    .await;
    let stop = Command::Interrupt { run };
    client
        .try_send(Request::new(
            RequestId::new(41),
            "stop",
            task.room.as_str(),
            stop,
        ))
        .unwrap();
    until(&client, |event| {
        matches!(event, Event::RunFinished { .. }).then_some(())
    })
    .await;
    client
        .try_send(invoke(42, &task.room, "tasks", "previous"))
        .unwrap();
    snapshot_of(&client, "main").await;
    let cost = until(&client, |event| match event {
        Event::RelatedCostUpdated {
            cost: Some(cost), ..
        } if !cost.complete => Some(cost.clone()),
        _ => None,
    })
    .await;
    assert!(cost.micros >= 3_000, "{cost:?}");
    drop(client);
    view.await.unwrap().unwrap();
    app.close().await.unwrap();
    server.abort();
}

/// Creates the room of a task of an earlier launch.
async fn seeded_task(app: &Application, id: &str, parent: &str, room: &str, agent: &str) {
    let metadata = roomkit::model::Metadata::from_iter([
        ("task_id".to_owned(), json!(id)),
        ("parent_room_id".to_owned(), json!(parent)),
        ("task_agent_id".to_owned(), json!(agent)),
        ("task_input".to_owned(), json!("Review the diff")),
        ("task_status".to_owned(), json!("completed")),
    ]);
    let options = roomkit::CreateRoomOptions {
        room_id: Some(roomkit::model::RoomId::new(room).unwrap()),
        metadata,
        ..Default::default()
    };
    app.kit.create_room(options).await.unwrap();
    app.profiles.prepare_target(&app.kit, agent).await.unwrap();
    let agent = ChannelId::new(agent).unwrap();
    let binding = roomkit::AttachChannelOptions {
        category: Some(roomkit::model::ChannelCategory::Intelligence),
        ..Default::default()
    };
    app.kit
        .attach_channel(&roomkit::model::RoomId::new(room).unwrap(), &agent, binding)
        .await
        .unwrap();
}

/// An agent that no longer exists leaves its room readable: the view says why
/// nothing can be written there.
#[tokio::test]
async fn a_task_of_a_deleted_agent_still_opens() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (base, _requests, server) = attentive_worker().await;
    let settings = settings(project.path(), state.path(), &base);
    with_a_reviewer(&settings).await;
    let app = Application::open(&settings, String::new()).await.unwrap();
    seeded_task(&app, "ghost", "main", "main::task-ghost", "review").await;
    let path = app._storage.database.with_file_name("agents.json");
    app.close().await.unwrap();
    drop(app);
    let mut document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    document["agents"].as_object_mut().unwrap().remove("review");
    std::fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();

    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, host) = connection(NonZeroUsize::new(1_024).unwrap());
    let router = app.router(&settings.workspace);
    let view = tokio::spawn(app.session().run_with_commands(host, router));
    snapshot_of(&client, "main").await;
    let tasks = app.profiles.resources.tasks.clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        while tasks.of("main").is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    open_room(&client, &tasks.of("main").remove(0).1).await;
    drop(client);
    view.await.unwrap().unwrap();
    app.close().await.unwrap();
    server.abort();
}

/// A task handed on in an earlier launch shows its conversation's mode: the
/// chain up to the conversation is known from opening it.
#[tokio::test]
async fn a_nested_task_of_an_earlier_launch_shows_its_conversation_s_mode() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (base, _requests, server) = attentive_worker().await;
    let settings = settings(project.path(), state.path(), &base);
    with_a_reviewer(&settings).await;
    let app = Application::open(&settings, String::new()).await.unwrap();
    seeded_task(&app, "a", "main", "main::task-a", "review").await;
    seeded_task(&app, "b", "main::task-a", "main::task-a::task-b", "review").await;
    app.policy.set("main", "plan").unwrap();
    let (client, host) = connection(NonZeroUsize::new(1_024).unwrap());
    let router = app.router(&settings.workspace);
    let view = tokio::spawn(app.session().run_with_commands(host, router));
    snapshot_of(&client, "main").await;
    let tasks = app.profiles.resources.tasks.clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        while tasks.of("main").len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let nested = tasks.find("main", "b").unwrap();
    client
        .try_send(invoke(30, "main", "tasks", &nested.id))
        .unwrap();
    let panel = until(&client, |event| match event {
        Event::Panel { panel, .. } => Some(panel.clone()),
        _ => None,
    })
    .await;
    let room = format!("{}:room:{}", panel.id, nested.id);
    client.try_send(invoke(31, "main", "tasks", &room)).unwrap();
    let opened = snapshot_of(&client, &nested.room).await;
    assert_eq!(opened.mode.unwrap().selected, "plan");
    drop(client);
    view.await.unwrap().unwrap();
    app.close().await.unwrap();
    server.abort();
}

/// What a worker that writes a file needs: an approval, in a view left on
/// the conversation, for a task handed to `review`.
struct Asking {
    app: Application,
    client: Client,
    view: tokio::task::JoinHandle<std::result::Result<(), roomkit_tui_engine::AdapterError>>,
    server: tokio::task::JoinHandle<()>,
    requests: Arc<Mutex<Vec<Value>>>,
    permission: roomkit_tui::protocol::Permission,
    project: tempfile::TempDir,
    _state: tempfile::TempDir,
}

async fn a_worker_asking() -> Asking {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (base, requests, server) = super::server(task_for("review")).await;
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
    let permission = until(&client, |event| match event {
        Event::PermissionRequested {
            conversation,
            permission,
        } => {
            assert_eq!(conversation.as_str(), "main");
            Some(permission.clone())
        }
        _ => None,
    })
    .await;
    Asking {
        app,
        client,
        view,
        server,
        requests,
        permission,
        project,
        _state: state,
    }
}

impl Asking {
    async fn close(self) {
        drop(self.client);
        self.view.await.unwrap().unwrap();
        self.app.close().await.unwrap();
        self.server.abort();
    }

    fn decide(&self, decision: Decision) {
        let command = Command::Decide {
            permission: self.permission.id.clone(),
            decision,
        };
        self.client
            .try_send(Request::new(RequestId::new(5), "decide", "main", command))
            .unwrap();
    }
}

/// The worker's approval comes to the conversation the person is looking at,
/// named after the agent and its room; allowed, the action runs.
#[tokio::test]
async fn a_worker_s_approval_comes_to_the_view_and_runs_once_allowed() {
    let asking = a_worker_asking().await;
    let task = asking.app.profiles.resources.tasks.of("main").remove(0).1;
    assert!(
        asking.permission.title.starts_with("Review asks: "),
        "{}",
        asking.permission.title
    );
    assert_eq!(
        asking.permission.origin.as_ref().map(|room| room.as_str()),
        Some(task.room.as_str())
    );
    asking.decide(Decision::Allow);
    until_handed_back(&asking.requests).await;
    assert!(asking.project.path().join("worker.txt").exists());
    // The decision is kept with the room it was asked in, opened or not.
    let room = roomkit::model::RoomId::new(&task.room).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let options = roomkit::GetTimelineOptions {
                limit: 200,
                ..Default::default()
            };
            let rows = asking.app.kit.get_timeline(&room, options).await.unwrap();
            if rows
                .iter()
                .any(|row| row.metadata.contains_key("roomkit_tui_approval"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    asking.close().await;
}

/// Refused, the action does not run, and the worker goes on to its result.
#[tokio::test]
async fn a_worker_s_approval_refused_stops_only_that_action() {
    let asking = a_worker_asking().await;
    asking.decide(Decision::Deny);
    until_handed_back(&asking.requests).await;
    assert!(!asking.project.path().join("worker.txt").exists());
    let task = asking.app.profiles.resources.tasks.of("main").remove(0).1;
    tokio::time::timeout(Duration::from_secs(10), async {
        while asking.app.profiles.resources.tasks.running() > 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let ended = asking.app.profiles.resources.tasks.find("main", &task.id);
    assert!(ended.unwrap().state().starts_with("completed"));
    asking.close().await;
}

/// Stopping the task withdraws its card.
#[tokio::test]
async fn stopping_a_task_withdraws_its_approval() {
    let asking = a_worker_asking().await;
    let task = asking.app.profiles.resources.tasks.of("main").remove(0).1;
    let options = roomkit::CancelTaskOptions::default();
    assert!(asking.app.kit.cancel_task(&task.id, options).await);
    let id = asking.permission.id.clone();
    until(&asking.client, |event| match event {
        Event::PermissionResolved { permission, .. } if *permission == id => Some(()),
        _ => None,
    })
    .await;
    assert!(!asking.project.path().join("worker.txt").exists());
    asking.close().await;
}

/// The card's key opens the room it was asked in, where it is shown again.
#[tokio::test]
async fn a_worker_s_approval_is_shown_again_in_its_own_room() {
    let asking = a_worker_asking().await;
    let task = asking.app.profiles.resources.tasks.of("main").remove(0).1;
    let target = format!("open {}", task.room);
    asking
        .client
        .try_send(invoke(6, "main", "tasks", &target))
        .unwrap();
    snapshot_of(&asking.client, &task.room).await;
    let id = asking.permission.id.clone();
    until(&asking.client, |event| match event {
        Event::PermissionRequested { permission, .. } if permission.id == id => Some(()),
        _ => None,
    })
    .await;
    let command = Command::Decide {
        permission: id,
        decision: Decision::Allow,
    };
    asking
        .client
        .try_send(Request::new(
            RequestId::new(7),
            "decide",
            task.room.as_str(),
            command,
        ))
        .unwrap();
    until_handed_back(&asking.requests).await;
    assert!(asking.project.path().join("worker.txt").exists());
    // Decided in the worker's own room, it is kept there too: the worker's
    // turn is no turn of the view's to store it as it ends.
    let room = roomkit::model::RoomId::new(&task.room).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let options = roomkit::GetTimelineOptions {
                limit: 200,
                ..Default::default()
            };
            let rows = asking.app.kit.get_timeline(&room, options).await.unwrap();
            if rows
                .iter()
                .any(|row| row.metadata.contains_key("roomkit_tui_approval"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    asking.close().await;
}

/// From a task's room the way back is offered: a command that says so, an
/// entry in the sidebar and in `/tasks`, and a word on arriving.
#[tokio::test]
async fn a_task_s_room_says_how_to_go_back() {
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
    client.try_send(invoke(9, "main", "back", "")).unwrap();
    until(&client, |event| match event {
        Event::Reply { request, error } if *request == RequestId::new(9) => {
            assert!(error.is_some(), "Already in the conversation");
            Some(())
        }
        _ => None,
    })
    .await;
    client
        .try_send(submit(1, "main", "code", "Review the diff"))
        .unwrap();
    until_requested(&requests, |request| request.to_string().contains(WORKER)).await;
    let task = app.profiles.resources.tasks.of("main").remove(0).1;
    client.try_send(invoke(2, "main", "tasks", "next")).unwrap();
    let opened = snapshot_of(&client, &task.room).await;
    assert!(opened.title.contains("/back"), "{}", opened.title);
    // Alt+Left moves to the previous stop, which may be another task's room.
    assert!(!opened.title.contains("Alt+Left"), "{}", opened.title);

    let (mut told, mut offered) = (false, false);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !(told && offered) {
            match client.try_recv() {
                Ok(Event::Notice { text, .. }) => told |= text.contains("/back"),
                Ok(Event::SidebarChanged {
                    sidebar: Some(sidebar),
                    ..
                }) => offered |= format!("{sidebar:?}").contains("Back to the conversation"),
                Ok(_) => {}
                Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
    })
    .await
    .unwrap();

    client.try_send(invoke(3, &task.room, "tasks", "")).unwrap();
    let panel = until(&client, |event| match event {
        Event::Panel { panel, .. } => Some(panel.clone()),
        _ => None,
    })
    .await;
    assert_eq!(panel.actions[0].value, "open main");
    assert_eq!(panel.actions[0].label, "Back to the conversation");

    client.try_send(invoke(4, &task.room, "back", "")).unwrap();
    snapshot_of(&client, "main").await;

    let task_rooms: Vec<String> = crate::sessions::commands()
        .into_iter()
        .map(|spec| spec.name)
        .collect();
    assert!(task_rooms.contains(&"back".to_owned()));
    let tasks = app.profiles.resources.tasks.clone();
    let stop = roomkit::CancelTaskOptions::default();
    assert!(
        app.kit
            .cancel_task(&tasks.of("main").remove(0).1.id, stop)
            .await
    );
    drop(client);
    view.await.unwrap().unwrap();
    app.close().await.unwrap();
    server.abort();
}
