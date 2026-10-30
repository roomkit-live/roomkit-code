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
    headers: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
struct Fixture {
    answer: String,
    calls: Vec<(&'static str, Value)>,
    usage: Vec<Option<Value>>,
    requests: Arc<Mutex<Vec<Value>>>,
    headers: Arc<Mutex<Vec<String>>>,
}

impl Mock {
    async fn start(calls: Vec<(&'static str, Value)>) -> Self {
        Self::with_answer(calls, "Project inspection complete.").await
    }

    async fn with_answer(calls: Vec<(&'static str, Value)>, answer_text: &str) -> Self {
        Self::with_reports(calls, answer_text, vec![]).await
    }

    async fn with_reports(
        calls: Vec<(&'static str, Value)>,
        answer_text: &str,
        usage: Vec<Option<Value>>,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let headers = Arc::new(Mutex::new(Vec::new()));
        let router = Router::new()
            .route("/v1/chat/completions", post(answer))
            .route(
                "/v1/models",
                axum::routing::get(|| async {
                    Json(json!({"data":[{"id":"discovered-model","max_model_len":32768}]}))
                }),
            )
            .with_state(Fixture {
                answer: answer_text.into(),
                calls,
                usage,
                requests: requests.clone(),
                headers: headers.clone(),
            });
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            base_url,
            requests,
            headers,
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
    headers: axum::http::HeaderMap,
    Json(request): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    fixture.headers.lock().unwrap().push(
        headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default()
            .into(),
    );
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
        None => (json!({"content":fixture.answer}), "stop"),
    };
    let report = fixture
        .usage
        .get(index)
        .and_then(Option::as_ref)
        .map(|usage| format!("data: {}\n\n", json!({"choices":[], "usage":usage})))
        .unwrap_or_default();
    let body = format!(
        "data: {}\n\ndata: {}\n\n{report}data: [DONE]\n\n",
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
    turn_operation(app, "inspect-project").await
}

async fn turn_operation(app: &Application, operation: &str) -> Session {
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    let session = tokio::time::timeout(Duration::from_secs(15), async {
        let mut session = Session::default();
        let mut submitted = false;
        loop {
            match client.try_recv() {
                Ok(event) => {
                    if let Event::RunFinished { outcome, .. } = &event {
                        assert_eq!(*outcome, RunOutcome::Completed);
                    }
                    // A streamed run can finish before admission/delivery cleanup.
                    // Reattach or edit profiles only at the operation boundary.
                    let finished = if let Event::OperationFinished {
                        operation: id,
                        outcome,
                        ..
                    } = &event
                    {
                        assert_eq!(id.0, operation);
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
                                operation: operation.into(),
                                conversation: snapshot.conversation.clone(),
                                command: Command::Submit {
                                    agent: snapshot.selected_agent.clone(),
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
    assert!(session.can_interrupt);
    assert!(
        session
            .commands
            .iter()
            .any(|command| command.name == "effort")
    );
    assert!(
        session
            .commands
            .iter()
            .any(|command| command.name == "diagnostics")
    );
    app.close().await.unwrap();
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
    app.close().await.unwrap();
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
    assert_eq!(
        names,
        crate::profiles::model::TOOLS.iter().copied().collect()
    );
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
async fn resumed_history_cannot_replace_the_current_wire_inventory_or_host_catalogue() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let stale = "I only have read-only tools. Shell is unavailable and all attempts failed.";
    let old = Mock::with_answer(vec![], stale).await;
    let settings = settings(project.path(), state.path(), &old.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    turn(&app).await;
    app.close().await.unwrap();
    drop(app);

    let current = Mock::start(vec![]).await;
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let provider_id = saved_provider_id(&settings);
    edit_profile(
        &mut app,
        "providers",
        &format!("edit:{provider_id}"),
        &[("url", current.base_url.as_str())],
    )
    .await;
    turn_operation(&app, "check-current-capabilities").await;
    let wire = current.requests.lock().unwrap()[0].clone();
    let inventory = app.profiles.tools(app.view.agent.as_str()).await;
    let expected: BTreeSet<_> = inventory.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(expected.len(), 19);
    let declared: BTreeSet<_> = wire["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(declared, expected);
    let messages = wire["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    let prompt = messages[0]["content"].as_str().unwrap();
    assert!(prompt.contains("Earlier assistant statements"));
    let inventory = prompt.split("Current capability inventory").nth(1).unwrap();
    for name in &expected {
        assert!(inventory.contains(name));
    }
    assert!(
        messages.iter().any(|message| message["role"] == "assistant"
            && message["content"]
                .as_str()
                .is_some_and(|text| text.contains(stale))),
        "Preserve the user's history while supplying current capabilities separately"
    );

    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: settings.workspace.clone(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
    };
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run_with_commands(host, handler));
    assert!(matches!(event(&client).await, Event::Snapshot(_)));
    let Event::Choices { choices, .. } = command_event(&client, "main", "tools", "", 20).await
    else {
        panic!("Expected tool catalogue");
    };
    assert_eq!(
        choices
            .iter()
            .map(|choice| choice.value.as_str())
            .collect::<BTreeSet<_>>(),
        expected
    );
    assert!(choices.iter().all(|choice| !choice.description.is_empty()));
    assert!(
        matches!(command_event(&client, "main", "tools", "shell", 21).await, Event::Notice { text } if text.contains("no tool executed"))
    );
    assert_eq!(
        current.requests.lock().unwrap().len(),
        1,
        "/tools never asks the model or executes a tool"
    );
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}

#[tokio::test]
async fn unregistered_tools_and_outside_reads_have_no_effect_or_disclosure() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("project");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(fixture.path().join("outside"), "outside-private-fixture").unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![
        (
            "unregistered_tool",
            json!({"path":"marker", "content":"unexpected"}),
        ),
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
    app.close().await.unwrap();
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        !serde_json::to_string(&*requests)
            .unwrap()
            .contains("outside-private-fixture")
    );
}

async fn event(client: &roomkit_tui::Client) -> Event {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match client.try_recv() {
                Ok(event) => return event,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(1)).await
                }
                Err(error) => panic!("Unexpected disconnection: {error}"),
            }
        }
    })
    .await
    .unwrap()
}

fn command(client: &roomkit_tui::Client, id: u64, room: &str, command: Command) {
    client
        .try_send(Request {
            id: RequestId(id),
            operation: format!("operation-{id}").as_str().into(),
            conversation: room.into(),
            command,
        })
        .unwrap();
}

async fn approval(client: &roomkit_tui::Client) -> roomkit_tui::protocol::Permission {
    loop {
        if let Event::PermissionRequested { permission, .. } = event(client).await {
            return permission;
        }
    }
}

#[tokio::test]
async fn ask_gates_real_changes_and_yolo_never_auto_approves_an_existing_request() {
    use roomkit_tui::protocol::Decision;
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let source = project.path().join("source.txt");
    std::fs::write(&source, "before\n").unwrap();
    let mock = Mock::start(vec![
        ("write", json!({"path":"denied.txt", "content":"never"})),
        ("edit", json!({"path":"source.txt", "old_text":"before", "new_text":"after"})),
        ("patch", json!({"changes":[{"op":"add", "path":"preview.txt", "content":"preview"}]})),
        ("patch", json!({"dry_run":false, "changes":[{"op":"add", "path":"applied.txt", "content":"applied"}]})),
        ("write", json!({"path":"yolo.txt", "content":"automatic"})),
        ("shell", json!({"command":"printf 'tested' > shell.txt; printf 'test output'"})),
    ]).await;
    let settings = settings(project.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: settings.workspace.clone(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
    };
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run_with_commands(host, handler));
    let Event::Snapshot(initial) = event(&client).await else {
        panic!("Expected snapshot");
    };
    assert_eq!(initial.mode.unwrap().selected, "ask");
    command(
        &client,
        1,
        "main",
        Command::Submit {
            agent: "code".into(),
            text: "Change the fixture".into(),
        },
    );
    let denied = approval(&client).await;
    assert!(denied.detail.contains("denied.txt") && denied.detail.contains("never"));
    assert!(!project.path().join("denied.txt").exists());
    // Incorrect room identity cannot approve an otherwise valid request.
    command(
        &client,
        2,
        "another-room",
        Command::Decide {
            permission: denied.id.clone(),
            decision: Decision::Allow,
        },
    );
    assert!(matches!(
        event(&client).await,
        Event::Reply { error: Some(_), .. }
    ));
    assert!(!project.path().join("denied.txt").exists());
    command(
        &client,
        3,
        "main",
        Command::Decide {
            permission: denied.id.clone(),
            decision: Decision::Deny,
        },
    );
    let edit = approval(&client).await;
    assert!(edit.detail.contains("before") && edit.detail.contains("after"));
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "before\n");
    command(
        &client,
        4,
        "main",
        Command::Invoke {
            name: "mode".into(),
            arguments: "yolo".into(),
        },
    );
    loop {
        if let Event::ModeChanged { mode, .. } = event(&client).await {
            assert_eq!(mode.selected, "yolo");
            break;
        }
    }
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "before\n",
        "Mode changes do not resolve existing approvals"
    );
    assert_eq!(mock.requests.lock().unwrap().len(), 2);
    // A previous answer cannot be replayed against a new action.
    command(
        &client,
        5,
        "main",
        Command::Decide {
            permission: denied.id,
            decision: Decision::Allow,
        },
    );
    assert!(matches!(
        event(&client).await,
        Event::Reply { error: Some(_), .. }
    ));
    command(
        &client,
        6,
        "main",
        Command::Decide {
            permission: edit.id,
            decision: Decision::Allow,
        },
    );
    loop {
        match event(&client).await {
            Event::PermissionRequested { .. } => panic!("Yolo must not ask for subsequent calls"),
            Event::RunFinished { outcome, .. } => {
                assert_eq!(outcome, RunOutcome::Completed);
                break;
            }
            _ => {}
        }
    }
    assert!(!project.path().join("denied.txt").exists());
    assert!(!project.path().join("preview.txt").exists());
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "after\n");
    assert_eq!(
        std::fs::read_to_string(project.path().join("applied.txt")).unwrap(),
        "applied"
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("yolo.txt")).unwrap(),
        "automatic"
    );
    assert_eq!(
        std::fs::read_to_string(project.path().join("shell.txt")).unwrap(),
        "tested"
    );
    command(
        &client,
        7,
        "main",
        Command::Invoke {
            name: "mode".into(),
            arguments: "ask".into(),
        },
    );
    loop {
        if let Event::ModeChanged { mode, .. } = event(&client).await {
            assert_eq!(mode.selected, "ask");
            break;
        }
    }
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let snapshot = app.session().snapshot().await.unwrap();
    assert_eq!(snapshot.mode.unwrap().selected, "ask");
    assert!(snapshot.items.iter().any(|item| matches!(item, Item::Tool {call, ..} if call.name == "write" && call.state == ToolState::Denied)));
    assert_eq!(
        mock.requests.lock().unwrap().len(),
        7,
        "Restoration never reexecutes the changes"
    );
    app.close().await.unwrap();
}

#[tokio::test]
async fn ask_shell_and_patch_require_approval_but_patch_preview_does_not() {
    use roomkit_tui::protocol::Decision;
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![
        (
            "patch",
            json!({"changes":[{"op":"add", "path":"file", "content":"new"}]}),
        ),
        (
            "patch",
            json!({"dry_run":false, "changes":[{"op":"add", "path":"file", "content":"new"}]}),
        ),
        ("shell", json!({"command":"printf 'approved' > shell.txt"})),
    ])
    .await;
    let app = Application::open(
        &settings(project.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    event(&client).await;
    command(
        &client,
        1,
        "main",
        Command::Submit {
            agent: "code".into(),
            text: "Test changes".into(),
        },
    );
    let patch = approval(&client).await;
    assert_eq!(patch.title, "Allow patch?");
    assert!(patch.detail.contains("\"dry_run\": false"));
    assert_eq!(
        mock.requests.lock().unwrap().len(),
        2,
        "Preview did not wait for approval"
    );
    assert!(!project.path().join("file").exists());
    command(
        &client,
        2,
        "main",
        Command::Decide {
            permission: patch.id,
            decision: Decision::Deny,
        },
    );
    let shell = approval(&client).await;
    assert_eq!(shell.title, "Allow shell?");
    assert!(shell.detail.contains("printf") && shell.detail.contains("not sandboxed"));
    assert!(!project.path().join("shell.txt").exists());
    command(
        &client,
        3,
        "main",
        Command::Decide {
            permission: shell.id,
            decision: Decision::Allow,
        },
    );
    loop {
        if matches!(event(&client).await, Event::RunFinished { .. }) {
            break;
        }
    }
    assert!(!project.path().join("file").exists());
    assert_eq!(
        std::fs::read_to_string(project.path().join("shell.txt")).unwrap(),
        "approved"
    );
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
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
    app.close().await.unwrap();
    drop(app);
    settings.session = Some("separate".into());
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert!(app.session().snapshot().await.unwrap().items.is_empty());
    app.close().await.unwrap();
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
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
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
    app.close().await.unwrap();
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
                    | Event::Notice { .. }
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
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
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
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_ne!(app.view.room.as_str(), first);
    assert_eq!(
        app.view.room.as_str(),
        second,
        "An unused draft is reused across launches"
    );
    assert!(app.session().snapshot().await.unwrap().items.is_empty());
    assert_eq!(
        mock.requests.lock().unwrap().len(),
        1,
        "Navigation must not invoke the model"
    );
    app.close().await.unwrap();
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
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
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
    app.close().await.unwrap();
}

#[tokio::test]
async fn outline_and_targeted_read_report_context_separately_from_consumption() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let source =
        "pub fn first() -> u8 { 1 }\npub fn second() -> u8 { 2 }\npub fn third() -> u8 { 3 }\n";
    std::fs::write(project.path().join("sample.rs"), source).unwrap();
    let mock = Mock::with_reports(vec![
        ("code_outline", json!({"path":"sample.rs"})),
        ("read", json!({"path":"sample.rs", "offset":2, "limit":1})),
    ], "The second function returns 2.", vec![
        Some(json!({"prompt_tokens":200,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":100}})),
        Some(json!({"prompt_tokens":300,"completion_tokens":30,"prompt_tokens_details":{"cached_tokens":200}})),
        Some(json!({"prompt_tokens":400,"completion_tokens":40,"prompt_tokens_details":{"cached_tokens":300}})),
    ]).await;
    let mut settings = settings(project.path(), state.path(), &mock.base_url);
    settings.context_window = Some(1024);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let session = turn(&app).await;
    assert_eq!(session.context.as_ref().unwrap().tokens, 440);
    assert_eq!(session.context.as_ref().unwrap().capacity, Some(1024));
    let usage = session.usage.as_ref().unwrap();
    assert_eq!(
        (
            usage.input_tokens,
            usage.output_tokens,
            usage.cached_input_tokens
        ),
        (Some(900), Some(90), 600)
    );
    assert!(usage.complete);
    let screen = roomkit_tui::render::screen(
        &session,
        240,
        45,
        &crate::tool_views::renderers(project.path()),
    );
    let text = screen.lines.join("\n");
    assert!(text.contains("3 symbols") && text.contains("pub fn first()"));
    assert!(!text.contains("\"symbols\""));
    assert!(text.contains("context 440/1024"));
    assert_eq!(
        std::fs::read_to_string(project.path().join("sample.rs")).unwrap(),
        source
    );
    let requests = mock.requests.lock().unwrap().clone();
    let result = requests[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|message| message["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(result.contains("pub fn second()"));
    assert!(!result.contains("pub fn first()") && !result.contains("pub fn third()"));
    app.close().await.unwrap();
    drop(app);
    let resumed = Application::open(&settings, String::new()).await.unwrap();
    let restored = resumed.session().snapshot().await.unwrap();
    assert_eq!(restored.context, session.context);
    assert_eq!(restored.usage, session.usage);
    assert_eq!(mock.requests.lock().unwrap().len(), 3);
    resumed.kit.close().await.unwrap();
}

#[tokio::test]
async fn missing_last_usage_is_unknown_and_previous_round_totals_are_partial() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::with_reports(
        vec![("list", json!({"path":"."}))],
        "Done.",
        vec![
            Some(json!({"prompt_tokens":100,"completion_tokens":10})),
            None,
        ],
    )
    .await;
    let settings = settings(project.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let session = turn(&app).await;
    assert!(session.context.is_none());
    let usage = session.usage.unwrap();
    assert_eq!(usage.input_tokens, Some(100));
    assert!(!usage.complete);
    app.close().await.unwrap();
}

#[tokio::test]
async fn provider_timeouts_and_refusals_have_actionable_private_diagnostics() {
    for (status, delay, expected) in [
        (
            axum::http::StatusCode::OK,
            Duration::from_secs(3),
            "timed out",
        ),
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Duration::ZERO,
            "access denied",
        ),
        (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Duration::ZERO,
            "quota",
        ),
    ] {
        let project = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let router = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(delay).await;
                    (
                        status,
                        Json(json!({"error":{"message":"secret-key and private server details"}})),
                    )
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut settings = settings(project.path(), state.path(), &base);
        settings.provider_timeout_seconds = 1;
        let app = Application::open(&settings, "secret-key".into())
            .await
            .unwrap();
        let (client, host) = connection(NonZeroUsize::new(256).unwrap());
        let task = tokio::spawn(app.session().run(host));
        let Event::Snapshot(snapshot) = event(&client).await else {
            panic!("Expected snapshot")
        };
        command(
            &client,
            1,
            "main",
            Command::Submit {
                agent: "code".into(),
                text: "Inspect".into(),
            },
        );
        let mut session = Session::default();
        session.apply(Event::Snapshot(snapshot));
        loop {
            let next = event(&client).await;
            let done = matches!(
                next,
                Event::RunFinished {
                    outcome: RunOutcome::Failed,
                    ..
                }
            );
            session.apply(next);
            if done {
                break;
            }
        }
        assert!(session.notice.contains(expected), "{}", session.notice);
        assert!(
            !session.notice.contains("secret-key") && !session.notice.contains("private server")
        );
        assert!(session.context.is_none());
        // ON_ERROR runs asynchronously; engine close drains its observers.
        drop(client);
        task.await.unwrap().unwrap();
        app.close().await.unwrap();
        let log =
            std::fs::read_to_string(app._storage.database.with_file_name("diagnostics.jsonl"))
                .unwrap();
        assert!(log.contains("engine_error"), "{log}");
        assert!(log.contains(expected), "{log}");
        assert!(!log.contains("secret-key") && !log.contains("private server details"));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "failures are not silently retried"
        );
        server.abort();
    }
}

#[tokio::test]
async fn unused_automatic_drafts_are_reused_without_adopting_named_sessions() {
    use roomkit::ConversationStore;
    use roomkit_cli_roomkit::{CommandHandler, CommandResult};
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let mut settings = settings(project.path(), state.path(), &mock.base_url);
    settings.session = None;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let draft = app.view.room.clone();
    crate::sessions::create(
        &app.kit,
        app.channel.id(),
        &app.view.agent,
        Some("named-empty"),
        false,
    )
    .await
    .unwrap();
    app.close().await.unwrap();
    drop(app);
    for launch in 0..3 {
        let app = Application::open(&settings, String::new()).await.unwrap();
        assert_eq!(app.view.room, draft);
        let handler = crate::sessions::Sessions {
            kit: app.kit.clone(),
            workspace: settings.workspace.clone(),
            channel: app.channel.id().clone(),
            policy: app.policy.clone(),
            profiles: app.profiles.clone(),
        };
        let request = Request {
            id: RequestId(1),
            operation: "empty-new".into(),
            conversation: draft.as_str().into(),
            command: Command::Invoke {
                name: "new".into(),
                arguments: String::new(),
            },
        };
        let CommandResult::Switch(destination) = handler
            .handle(request.clone(), app.view.clone())
            .await
            .unwrap()
        else {
            panic!("Expected unchanged empty draft");
        };
        assert_eq!(destination.room, draft);
        assert_eq!(
            app.kit
                .store()
                .list_rooms(&Default::default())
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(app.session().snapshot().await.unwrap().items.is_empty());
        if launch == 2 {
            turn(&app).await;
            let CommandResult::Switch(retried) =
                handler.handle(request, app.view.clone()).await.unwrap()
            else {
                panic!("Expected retry of the empty-draft command");
            };
            assert_eq!(
                retried.room, draft,
                "A delayed no-op retry must stay a no-op after a message"
            );
        }
        app.close().await.unwrap();
    }
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_ne!(
        app.view.room, draft,
        "A conversation with messages must never be reused as a draft"
    );
    assert_ne!(app.view.room.as_str(), "named-empty");
    assert!(app.session().snapshot().await.unwrap().items.is_empty());
    app.close().await.unwrap();
}

#[tokio::test]
async fn resume_skips_legacy_empty_sessions_before_applying_the_catalogue_limit() {
    use roomkit_cli_roomkit::{CommandHandler, CommandResult};
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(project.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    turn(&app).await;
    let used = app.view.room.clone();
    let mut current = app.view.clone();
    for index in 0..201 {
        current.room = crate::sessions::create(
            &app.kit,
            app.channel.id(),
            &app.view.agent,
            Some(&format!("legacy-empty-{index}")),
            false,
        )
        .await
        .unwrap();
    }
    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: settings.workspace.clone(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
    };
    let request = Request {
        id: RequestId(1),
        operation: "list-sessions".into(),
        conversation: current.room.as_str().into(),
        command: Command::Invoke {
            name: "resume".into(),
            arguments: String::new(),
        },
    };
    let CommandResult::Events(events) = handler.handle(request, current).await.unwrap() else {
        panic!("Expected session catalogue");
    };
    let Event::Choices { choices, .. } = &events[0] else {
        panic!("Expected choices")
    };
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].value, used.as_str());
    assert_eq!(choices[0].label, "Inspect the project.");
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    app.close().await.unwrap();
}

fn saved_provider_id(settings: &Settings) -> String {
    let json: Value = serde_json::from_slice(
        &std::fs::read(settings.state_dir.join("connections.json")).unwrap(),
    )
    .unwrap();
    json["profiles"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone()
}

async fn profile_command(
    app: &Application,
    command: Command,
) -> Result<roomkit_cli_roomkit::CommandResult> {
    Ok(app
        .profiles
        .handle(
            &app.kit,
            app.channel.id(),
            Request {
                id: RequestId(99),
                operation: "profile-operation".into(),
                conversation: app.view.room.as_str().into(),
                command,
            },
            app.view.clone(),
        )
        .await?)
}

async fn edit_profile(
    app: &mut Application,
    command: &str,
    action: &str,
    changes: &[(&str, &str)],
) {
    use roomkit_cli_roomkit::CommandResult;
    use roomkit_tui::protocol::FormValues;
    let CommandResult::Events(events) = profile_command(
        app,
        Command::Invoke {
            name: command.into(),
            arguments: action.into(),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected a form")
    };
    let form = events
        .into_iter()
        .find_map(|event| match event {
            Event::Form { form, .. } => Some(form),
            _ => None,
        })
        .unwrap();
    let mut values = FormValues(
        form.fields
            .iter()
            .map(|field| (field.name.clone(), field.value.clone()))
            .collect(),
    );
    for (name, value) in changes {
        values.0.insert((*name).into(), (*value).into());
    }
    let CommandResult::Switch(view) = profile_command(
        app,
        Command::SubmitForm {
            name: form.command,
            form: form.id,
            values: Box::new(values),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected refreshed view")
    };
    app.view = *view;
}

#[tokio::test]
async fn profiles_select_independent_providers_tools_and_auth_in_one_durable_conversation() {
    use roomkit_cli_roomkit::CommandResult;
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let first = Mock::with_answer(vec![], "First agent answer").await;
    let second = Mock::with_answer(vec![], "Second agent answer").await;
    let mut settings = settings(root.path(), state.path(), &first.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let room = app.view.room.clone();
    turn_operation(&app, "first-agent-turn").await;
    edit_profile(
        &mut app,
        "providers",
        "add",
        &[
            ("id", "secondary"),
            ("label", "Second connection"),
            ("url", &second.base_url),
            ("auth", "stored"),
            ("key", "fixture-private-key"),
        ],
    )
    .await;
    edit_profile(
        &mut app,
        "agents",
        "add",
        &[
            ("id", "reviewer"),
            ("label", "Reviewer"),
            ("provider", "secondary"),
            ("model", "second-model"),
            ("context", "32768"),
            (
                "instructions",
                "Review carefully with this distinct instruction.",
            ),
            ("tools", "read, code_outline"),
        ],
    )
    .await;
    assert_eq!(app.view.room, room);
    assert_eq!(app.view.agent.as_str(), "reviewer");
    let second_session = turn_operation(&app, "second-agent-turn").await;
    assert_eq!(first.requests.lock().unwrap().len(), 1);
    let request = second.requests.lock().unwrap()[0].clone();
    assert_eq!(
        second.headers.lock().unwrap().as_slice(),
        ["Bearer fixture-private-key"]
    );
    assert_eq!(request["model"], "second-model");
    assert!(
        request["messages"]
            .to_string()
            .contains("First agent answer")
    );
    assert!(
        request["messages"]
            .to_string()
            .contains("distinct instruction")
    );
    assert!(!request.to_string().contains("fixture-private-key"));
    let tools: BTreeSet<_> = request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools, BTreeSet::from(["read", "code_outline"]));
    let snapshot = second_session.snapshot.unwrap();
    assert!(
        snapshot
            .actors
            .iter()
            .any(|a| a.id.0 == "reviewer" && a.name == "Reviewer")
    );
    for (author, answer) in [
        ("code", "First agent answer"),
        ("reviewer", "Second agent answer"),
    ] {
        assert!(snapshot.items.iter().any(
            |item| matches!(item, Item::Message(m) if m.actor.0 == author && m.text == answer)
        ));
    }
    assert!(!format!("{snapshot:?}").contains("fixture-private-key"));
    let CommandResult::Switch(view) = profile_command(
        &app,
        Command::Invoke {
            name: "agents".into(),
            arguments: "select:code".into(),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected switch")
    };
    app.view = *view;
    turn_operation(&app, "back-to-first-agent").await;
    assert_eq!(first.requests.lock().unwrap().len(), 2);
    assert_eq!(second.requests.lock().unwrap().len(), 1);
    assert!(first.headers.lock().unwrap().iter().all(String::is_empty));
    let CommandResult::Switch(view) = profile_command(
        &app,
        Command::Invoke {
            name: "agents".into(),
            arguments: "select:reviewer".into(),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected switch")
    };
    app.view = *view;
    // Editing a stored-key connection without entering a key retains it.
    edit_profile(
        &mut app,
        "providers",
        "edit:secondary",
        &[("label", "Renamed connection")],
    )
    .await;
    app.close().await.unwrap();
    drop(app);
    settings.base_url.clear();
    settings.model.clear();
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(app.view.agent.as_str(), "reviewer");
    assert_eq!(app.view.context_capacity, Some(32768));
    let snapshot = app.session().snapshot().await.unwrap();
    assert!(snapshot.items.len() >= 6);
    assert!(!format!("{snapshot:?}").contains("fixture-private-key"));
    turn_operation(&app, "after-restart").await;
    assert_eq!(
        second.headers.lock().unwrap().last().unwrap(),
        "Bearer fixture-private-key"
    );
    app.close().await.unwrap();
}

#[tokio::test]
async fn unconfigured_start_can_create_profiles_without_a_dummy_model_or_endpoint() {
    use roomkit_cli_roomkit::CommandResult;
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let provider = Mock::start(vec![]).await;
    let mut settings = settings(root.path(), state.path(), &provider.base_url);
    settings.base_url.clear();
    settings.model.clear();
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    assert!(app.view.model.is_none());
    assert!(app.view.submission_error.is_some());
    edit_profile(
        &mut app,
        "providers",
        "add",
        &[("id", "local"), ("url", &provider.base_url)],
    )
    .await;
    let CommandResult::Events(events) = profile_command(
        &app,
        Command::Invoke {
            name: "providers".into(),
            arguments: "test:local".into(),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected catalogue")
    };
    assert!(
        matches!(&events[0], Event::Choices { choices, .. } if choices[0].value == "model:local:discovered-model")
    );
    edit_profile(
        &mut app,
        "providers",
        "model:local:discovered-model",
        &[("id", "assistant"), ("tools", "")],
    )
    .await;
    assert!(app.view.submission_error.is_none());
    assert_eq!(app.view.model.as_ref().unwrap().model, "discovered-model");
    turn_operation(&app, "first-configured-turn").await;
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    app.close().await.unwrap();
}

#[tokio::test]
async fn stale_or_conflicting_forms_cannot_replace_a_working_agent() {
    use roomkit_cli_roomkit::CommandResult;
    use roomkit_tui::protocol::FormValues;
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let provider = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &provider.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let open = || Command::Invoke {
        name: "agents".into(),
        arguments: "edit:code".into(),
    };
    let first = profile_command(&app, open()).await.unwrap();
    let second = profile_command(&app, open()).await.unwrap();
    let extract = |result| {
        let CommandResult::Events(events) = result else {
            panic!("Expected form")
        };
        events
            .into_iter()
            .find_map(|event| match event {
                Event::Form { form, .. } => Some(form),
                _ => None,
            })
            .unwrap()
    };
    let old = extract(first);
    let form = extract(second);
    assert_ne!(old.id, form.id);
    let mut values = FormValues(
        form.fields
            .iter()
            .map(|f| (f.name.clone(), f.value.clone()))
            .collect(),
    );
    assert!(
        profile_command(
            &app,
            Command::SubmitForm {
                name: "agents".into(),
                form: old.id,
                values: Box::new(values.clone())
            }
        )
        .await
        .is_err()
    );
    values.0.insert("model".into(), "should-not-be-used".into());
    let submit = Command::SubmitForm {
        name: "agents".into(),
        form: form.id.clone(),
        values: Box::new(values),
    };
    let mut wrong_view = app.view.clone();
    wrong_view.room = roomkit::model::RoomId::new("other-room").unwrap();
    assert!(
        app.profiles
            .handle(
                &app.kit,
                app.channel.id(),
                Request {
                    id: RequestId(1),
                    operation: "other-view".into(),
                    conversation: "other-room".into(),
                    command: submit.clone()
                },
                wrong_view
            )
            .await
            .is_err()
    );
    // Simulate an external editor changing the saved project while this form is open.
    let projects = std::fs::read_dir(state.path().join("projects")).unwrap();
    let file = projects
        .into_iter()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("agents.json");
    let mut saved: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    saved["agents"]["code"]["label"] = "External edit".into();
    std::fs::write(&file, serde_json::to_vec(&saved).unwrap()).unwrap();
    let error = profile_command(&app, submit).await.unwrap_err().to_string();
    assert!(error.contains("changed in another instance"));
    turn_operation(&app, "after-rejected-edit").await;
    assert_eq!(
        provider.requests.lock().unwrap()[0]["model"],
        "fixture-model"
    );
    // Reopening adopts the new disk revision and issues a fresh token.
    let reopened = extract(profile_command(&app, open()).await.unwrap());
    assert_eq!(
        reopened
            .fields
            .iter()
            .find(|f| f.name == "label")
            .unwrap()
            .value,
        "External edit"
    );
    app.close().await.unwrap();
}

mod catalogue;

mod processes;

mod accounts;
mod profile_deletion;

#[tokio::test]
async fn effort_selection_persists_reconfigures_requests_and_can_restore_provider_default() {
    use roomkit_cli_roomkit::CommandResult;
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    let room = app.view.room.clone();
    for effort in ["high", "default"] {
        let CommandResult::Switch(view) = profile_command(
            &app,
            Command::Invoke {
                name: "effort".into(),
                arguments: effort.into(),
            },
        )
        .await
        .unwrap() else {
            panic!("Expected refreshed view")
        };
        app.view = *view;
        assert_eq!(app.view.room, room);
        assert_eq!(
            app.view.model.as_ref().unwrap().effort.as_deref(),
            Some(effort)
        );
        turn_operation(&app, effort).await;
        let request = mock.requests.lock().unwrap().last().unwrap().clone();
        if effort == "default" {
            assert!(request.get("reasoning_effort").is_none());
        } else {
            assert_eq!(request["reasoning_effort"], effort);
        }
    }
    // The agent form uses the same persisted setting as the quick command.
    edit_profile(&mut app, "agents", "edit:code", &[("effort", "medium")]).await;
    assert!(
        profile_command(
            &app,
            Command::Invoke {
                name: "effort".into(),
                arguments: "unsupported".into()
            }
        )
        .await
        .is_err()
    );
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(
        app.view.model.as_ref().unwrap().effort.as_deref(),
        Some("medium")
    );
    turn_operation(&app, "after-restart").await;
    assert_eq!(
        mock.requests.lock().unwrap().last().unwrap()["reasoning_effort"],
        "medium"
    );
    app.close().await.unwrap();
}

mod images;
mod shell;
