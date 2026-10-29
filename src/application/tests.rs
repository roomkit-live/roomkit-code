//! Exercise the assembled product through HTTP, filesystem tools and its UI protocol.

#![expect(clippy::unwrap_used, reason = "fixture failures must fail the test")]
#![expect(
    clippy::panic,
    reason = "an unexpected disconnection must fail the test"
)]

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{Json, Router, extract::State, routing::post};
use clap::Parser;
use roomkit_tui::{
    Session,
    protocol::{Command, Event, Item, Request, RequestId, RunOutcome, ToolState},
};
use serde_json::{Value, json};

use super::*;
use crate::config::Cli;

struct Mock {
    base_url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
struct Fixture {
    calls: Vec<(&'static str, Value)>,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl Mock {
    async fn start(calls: Vec<(&'static str, Value)>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let router = Router::new()
            .route("/v1/chat/completions", post(answer))
            .with_state(Fixture {
                calls,
                requests: requests.clone(),
            });
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            base_url,
            requests,
            task,
        }
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn answer(
    State(fixture): State<Fixture>,
    Json(request): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    let index = {
        let mut requests = fixture.requests.lock().unwrap();
        let index = requests.len();
        requests.push(request);
        index
    };
    let (delta, reason) = match fixture.calls.get(index) {
        Some((name, args)) => (
            json!({"tool_calls":[{"index":0,"id":format!("call-{index}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]}),
            "tool_calls",
        ),
        None => (json!({"content":"Project inspection complete."}), "stop"),
    };
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":delta}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]})
    );
    ([("content-type", "text/event-stream")], body)
}

fn settings(workspace: &std::path::Path, state: &std::path::Path, base: &str) -> Settings {
    let cli = Cli::try_parse_from([
        "roomkit-code",
        workspace.to_str().unwrap(),
        "--state-dir",
        state.to_str().unwrap(),
        "--base-url",
        base,
        "--model",
        "fixture-model",
        "--session",
        "main",
    ])
    .unwrap();
    Settings::load(&cli).unwrap()
}

async fn turn(app: &Application) -> Session {
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    let session = tokio::time::timeout(Duration::from_secs(15), async {
        let mut session = Session::default();
        let mut submitted = false;
        loop {
            match client.try_recv() {
                Ok(event) => {
                    let finished = if let Event::RunFinished { outcome, .. } = &event {
                        assert_eq!(*outcome, RunOutcome::Completed);
                        true
                    } else {
                        false
                    };
                    session.apply(event);
                    if !submitted && let Some(snapshot) = &session.snapshot {
                        client
                            .try_send(Request {
                                id: RequestId(1),
                                operation: "inspect-project".into(),
                                conversation: snapshot.conversation.clone(),
                                command: Command::Submit {
                                    agent: "code".into(),
                                    text: "Inspect the project.".into(),
                                },
                            })
                            .unwrap();
                        submitted = true;
                    }
                    if finished {
                        break session;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(1)).await
                }
                Err(error) => panic!("Unexpected attachment failure: {error}"),
            }
        }
    })
    .await
    .unwrap();
    drop(client);
    task.await.unwrap().unwrap();
    session
}

#[tokio::test]
async fn real_tools_stream_results_and_resume_without_reexecution() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn forty_two() -> u8 { 42 }\n",
    )
    .unwrap();
    let mock = Mock::start(vec![
        ("list", json!({"path":"."})),
        ("glob", json!({"pattern":"**/*.rs"})),
        ("grep", json!({"pattern":"forty_two"})),
        ("read", json!({"path":"src/lib.rs"})),
    ])
    .await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let session = turn(&app).await.snapshot.unwrap();
    assert_eq!(
        session
            .items
            .iter()
            .filter(
                |item| matches!(item, Item::Tool { call, .. } if call.state == ToolState::Succeeded)
            )
            .count(),
        4
    );
    let final_id = session
        .items
        .iter()
        .find_map(|item| match item {
            Item::Message(message) if message.text == "Project inspection complete." => {
                Some(message.id.clone())
            }
            _ => None,
        })
        .unwrap();
    assert!(!session.can_interrupt);
    assert_eq!(session.commands.len(), 2);
    app.kit.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let restored = app.session().snapshot().await.unwrap();
    assert!(
        restored
            .items
            .iter()
            .any(|item| matches!(item, Item::Message(message) if message.id == final_id))
    );
    assert_eq!(
        restored
            .items
            .iter()
            .filter(|item| matches!(item, Item::Tool { .. }))
            .count(),
        4
    );
    app.kit.close().await.unwrap();
    let requests = mock.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        5,
        "Restoration must not reexecute tools or call the model"
    );
    let names: BTreeSet<_> = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, BTreeSet::from(["list", "glob", "grep", "read"]));
    assert!(
        requests[4]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool"
                && message["content"]
                    .as_str()
                    .is_some_and(|text| text.contains("pub fn forty_two()")))
    );
}

#[tokio::test]
async fn unregistered_writes_and_outside_reads_have_no_effect_or_disclosure() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(fixture.path().join("outside"), "outside-private-fixture").unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![
        ("write", json!({"path":"marker", "content":"unexpected"})),
        ("read", json!({"path":"../outside"})),
    ])
    .await;
    let app = Application::open(
        &settings(&root, state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    let session = turn(&app).await.snapshot.unwrap();
    assert!(!root.join("marker").exists());
    assert!(session.items.iter().all(
        |item| !matches!(item, Item::Tool { call, .. } if call.state == ToolState::Succeeded)
    ));
    app.kit.close().await.unwrap();
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        !serde_json::to_string(&*requests)
            .unwrap()
            .contains("outside-private-fixture")
    );
}

#[tokio::test]
async fn histories_are_scoped_to_project_and_session_and_storage_has_one_owner() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let mut settings = settings(first.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    turn(&app).await;
    assert!(Application::open(&settings, String::new()).await.is_err());
    app.kit.close().await.unwrap();
    drop(app);
    settings.session = Some("separate".into());
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert!(app.session().snapshot().await.unwrap().items.is_empty());
    app.kit.close().await.unwrap();
    drop(app);
    settings.workspace = second.path().canonicalize().unwrap();
    settings.session = Some("main".into());
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert!(app.session().snapshot().await.unwrap().items.is_empty());
    use roomkit_cli_roomkit::{CommandHandler, CommandResult};
    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: settings.workspace.clone(),
        channel: app.channel.id().clone(),
    };
    let request = Request {
        id: RequestId(1),
        operation: "project-catalogue".into(),
        conversation: "main".into(),
        command: Command::Invoke {
            name: "resume".into(),
            arguments: String::new(),
        },
    };
    let CommandResult::Events(events) = handler
        .handle(request.clone(), app.view.clone())
        .await
        .unwrap()
    else {
        panic!("Expected session catalogue");
    };
    assert!(
        matches!(&events[0], Event::Choices { choices, .. } if choices.is_empty()),
        "Other projects must not appear in the selector"
    );
    let request = Request {
        command: Command::Invoke {
            name: "resume".into(),
            arguments: "separate".into(),
        },
        ..request
    };
    assert!(handler.handle(request, app.view.clone()).await.is_err());
    app.kit.close().await.unwrap();
}

async fn command_event(
    client: &roomkit_tui::Client,
    conversation: &str,
    name: &str,
    arguments: &str,
    id: u64,
) -> Event {
    client
        .try_send(Request {
            id: RequestId(id),
            operation: format!("command-{id}").as_str().into(),
            conversation: conversation.into(),
            command: Command::Invoke {
                name: name.into(),
                arguments: arguments.into(),
            },
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match client.try_recv() {
                Ok(
                    event @ (Event::Snapshot(_)
                    | Event::Choices { .. }
                    | Event::Reply { error: Some(_), .. }),
                ) => return event,
                Ok(_) => {}
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(1)).await
                }
                Err(error) => panic!("Controller disconnected: {error}"),
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn fresh_launch_new_resume_and_retried_new_preserve_scoped_durable_history() {
    use roomkit::ConversationStore;
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let mut settings = settings(project.path(), state.path(), &mock.base_url);
    settings.session = None;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let first = app.view.room.to_string();
    let saved = turn(&app).await.snapshot.unwrap();
    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: settings.workspace.clone(),
        channel: app.channel.id().clone(),
    };
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run_with_commands(host, handler));
    // Wait for the authoritative initial projection before sending commands.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(client.try_recv(), Ok(Event::Snapshot(_))) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let Event::Snapshot(fresh) = command_event(&client, &first, "new", "", 1).await else {
        panic!("Expected a new session");
    };
    assert_ne!(fresh.conversation.0, first);
    assert!(fresh.items.is_empty());
    let second = fresh.conversation.0;
    let Event::Choices { choices, .. } = command_event(&client, &second, "resume", "", 2).await
    else {
        panic!("Expected the selector");
    };
    assert!(
        choices
            .iter()
            .any(|choice| choice.value == first && choice.label == "Inspect the project.")
    );
    assert!(!choices.iter().any(|choice| choice.value == second));
    let before = app
        .kit
        .store()
        .list_rooms(&Default::default())
        .await
        .unwrap()
        .len();
    assert!(matches!(
        command_event(&client, &second, "resume", "missing-session", 3).await,
        Event::Reply { error: Some(_), .. }
    ));
    assert_eq!(
        app.kit
            .store()
            .list_rooms(&Default::default())
            .await
            .unwrap()
            .len(),
        before
    );
    let Event::Snapshot(restored) = command_event(&client, &second, "resume", &first, 4).await
    else {
        panic!("Expected restored history");
    };
    assert_eq!(restored.items, saved.items);
    assert!(
        matches!(
            command_event(&client, &second, "new", "", 5).await,
            Event::Reply { error: Some(_), .. }
        ),
        "A stale conversation cannot create a room"
    );
    let Event::Snapshot(retried) = command_event(&client, &first, "new", "", 1).await else {
        panic!("Expected retry destination");
    };
    assert_eq!(retried.conversation.0, second);
    assert_eq!(
        app.kit
            .store()
            .list_rooms(&Default::default())
            .await
            .unwrap()
            .len(),
        before
    );
    drop(client);
    task.await.unwrap().unwrap();
    app.kit.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_ne!(app.view.room.as_str(), first);
    assert_ne!(app.view.room.as_str(), second);
    assert!(app.session().snapshot().await.unwrap().items.is_empty());
    assert_eq!(
        mock.requests.lock().unwrap().len(),
        1,
        "Navigation must not invoke the model"
    );
    app.kit.close().await.unwrap();
    drop(app);
    settings.session = Some(first.clone());
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(app.view.room.as_str(), first);
    assert_eq!(app.session().snapshot().await.unwrap().items, saved.items);
    use roomkit_cli_roomkit::{CommandHandler, CommandResult};
    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: settings.workspace.clone(),
        channel: app.channel.id().clone(),
    };
    let request = Request {
        id: RequestId(1),
        operation: "command-1".into(),
        conversation: first.as_str().into(),
        command: Command::Invoke {
            name: "new".into(),
            arguments: String::new(),
        },
    };
    let CommandResult::Switch(destination) =
        handler.handle(request, app.view.clone()).await.unwrap()
    else {
        panic!("Expected retry destination after restart");
    };
    assert_eq!(destination.room.as_str(), second);
    app.kit.close().await.unwrap();
}
