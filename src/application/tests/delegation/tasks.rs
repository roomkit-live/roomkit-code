//! The view lists the tasks its conversation's agents handed over, in its
//! sidebar, its background status and `/tasks`, and stops one that runs.

use roomkit_tui::protocol::{PanelSpec, SidebarContent};
use roomkit_tui_engine::{CommandHandler, CommandResult};

use super::*;

/// A model server whose worker lists the project, then never answers in time,
/// so its task runs until it is stopped; the delegating agent hands `review`
/// a task, then says it did.
async fn slow_worker() -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let router = Router::new().route(
        "/v1/chat/completions",
        post(move |Json(request): Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(request.clone());
                let answered = request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|message| message["role"] == "tool");
                let worker = request.to_string().contains(WORKER);
                if worker && answered {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
                let delta = if worker {
                    call("list", &json!({"path": "."}))
                } else if answered {
                    json!({"content": "Handed to the reviewer."})
                } else {
                    call("delegate_task", &task_for("review"))
                };
                let reason = if answered { "stop" } else { "tool_calls" };
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

/// Waits until the view's live events hold one `found` returns something for.
async fn until<T>(
    updates: &mut tokio::sync::watch::Receiver<Vec<Event>>,
    found: impl Fn(&Event) -> Option<T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(value) = updates.borrow_and_update().iter().find_map(&found) {
                return value;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

/// What `/tasks` with `arguments` opened in the view of `current`.
async fn tasks(
    handler: &crate::sessions::Sessions,
    current: &SessionConfig,
    arguments: &str,
) -> std::result::Result<PanelSpec, String> {
    let request = Request::new(
        RequestId::new(1),
        "tasks",
        current.room.as_str(),
        Command::Invoke {
            name: "tasks".into(),
            arguments: arguments.into(),
        },
    );
    match handler.handle(request, current.clone()).await {
        Ok(CommandResult::Events(events)) => match events.into_iter().next() {
            Some(Event::Panel { panel, .. }) => Ok(panel),
            other => panic!("Expected a panel: {other:?}"),
        },
        Ok(_) => panic!("Expected the tasks panel"),
        Err(rejection) => Err(format!("{rejection:?}")),
    }
}

/// The panel's action whose value ends with `action`.
fn action(panel: &PanelSpec, action: &str) -> String {
    let value = format!("{}:{action}", panel.id);
    let choice = panel.actions.iter().find(|choice| choice.value == value);
    choice
        .unwrap_or_else(|| panic!("{action}: {panel:?}"))
        .value
        .clone()
}

#[tokio::test]
async fn a_running_task_is_listed_and_stopped_from_the_view() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let (base, requests, server) = slow_worker().await;
    let settings = settings(project.path(), state.path(), &base);
    with_a_reviewer(&settings).await;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: project.path().into(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
        services: app.services.clone(),
    };
    assert!(handler.allow_while_busy("tasks"));
    let mut updates = handler.subscribe(app.view.clone()).unwrap();
    turn(&app).await;

    // The task runs: the background status counts it and opens /tasks, the
    // exit warning says closing stops it, the sidebar lists it.
    let status = until(&mut updates, |event| match event {
        Event::BackgroundChanged {
            status: Some(status),
            ..
        } => Some(status.clone()),
        _ => None,
    })
    .await;
    assert_eq!(
        (status.label.as_str(), status.command.as_str()),
        ("Tasks: 1 running", "tasks")
    );
    until(&mut updates, |event| match event {
        Event::ExitWarning {
            text: Some(text), ..
        } => (text == "1 delegated task will be stopped").then_some(()),
        _ => None,
    })
    .await;
    let section = until(&mut updates, |event| match event {
        Event::SidebarChanged {
            sidebar: Some(sidebar),
            ..
        } => sidebar
            .sections
            .iter()
            .find(|s| s.title == "Tasks")
            .cloned(),
        _ => None,
    })
    .await;
    let SidebarContent::Entries { entries } = &section.content else {
        panic!("Expected task entries: {section:?}");
    };
    assert_eq!(entries[0].label, "● Review");
    assert!(entries[0].detail.starts_with("running"), "{entries:?}");

    // Another conversation's view neither lists nor opens it.
    let mut other = app.view.clone();
    other.room = roomkit::model::RoomId::new("other").unwrap();
    let elsewhere = tasks(&handler, &other, "").await.unwrap();
    assert!(elsewhere.body.contains("No tasks."), "{}", elsewhere.body);
    let task = app.profiles.resources.tasks.of("main").remove(0).1;
    assert!(tasks(&handler, &other, &task.id).await.is_err());

    // /tasks lists it; opening it shows the task it was given.
    let list = tasks(&handler, &app.view, "").await.unwrap();
    assert!(list.body.contains("Review · running"), "{}", list.body);
    let open = tasks(
        &handler,
        &app.view,
        &action(&list, &format!("open:{}", task.id)),
    )
    .await
    .unwrap();
    assert!(
        open.title.starts_with("Task for Review · running"),
        "{}",
        open.title
    );
    assert!(open.body.contains("Review the diff"), "{}", open.body);

    // It shows the task's conversation as it goes on: what its agent was
    // handed, and the call it made.
    let followed = until(&mut updates, |event| match event {
        Event::PanelUpdated { panel, .. } if panel.body.contains("✓ list") => {
            Some(panel.body.clone())
        }
        _ => None,
    })
    .await;
    assert!(
        followed.contains("Its conversation, the latest last:"),
        "{followed}"
    );
    assert!(followed.contains("Task: "), "{followed}");

    // Stopping it cancels it: the worker never answers, and the view says so.
    let stopped = tasks(&handler, &app.view, &action(&open, "stop"))
        .await
        .unwrap();
    assert!(
        !stopped
            .actions
            .iter()
            .any(|choice| choice.label == "Stop task")
    );
    let notice = until(&mut updates, |event| match event {
        Event::Notice { text, .. } if text.starts_with("Task for Review cancelled") => {
            Some(text.clone())
        }
        _ => None,
    })
    .await;
    assert_eq!(notice, "Task for Review cancelled");
    until(&mut updates, |event| match event {
        Event::BackgroundChanged { status: None, .. } => Some(()),
        _ => None,
    })
    .await;
    let ended = app.profiles.resources.tasks.find("main", &task.id).unwrap();
    assert!(
        ended.state().starts_with("cancelled after"),
        "{}",
        ended.state()
    );
    // The worker made its call, then was stopped in its next request.
    let worker = requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.to_string().contains(WORKER))
        .count();
    assert_eq!(worker, 2);

    drop(updates);
    app.close().await.unwrap();
    server.abort();
}
