//! RMK-1230: a person writes while the agent works. The message goes to the
//! running turn, which reads it at its next step; written as the turn writes
//! its last answer, it gets a turn of its own, with the hooks of any turn.

use std::sync::atomic::{AtomicUsize, Ordering};

use roomkit::HookOptions;
use roomkit_tui::protocol::Decision;
use tokio::sync::{Notify, watch};

use super::hooks::{configure, enable_all, records};
use super::*;

/// Events until one satisfies `done`, that one included.
async fn until(client: &roomkit_tui::Client, done: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        let next = event(client).await;
        let found = done(&next);
        seen.push(next);
        if found {
            return seen;
        }
    }
}

fn submit(client: &roomkit_tui::Client, id: u64, text: &str) {
    let text = text.into();
    command(
        client,
        id,
        "main",
        Command::Submit {
            agent: "code".into(),
            text,
        },
    );
}

fn accepted(events: &[Event], id: u64) -> bool {
    events.iter().any(
        |e| matches!(e, Event::Reply { request, error: None } if *request == RequestId::new(id)),
    )
}

fn finished(event: &Event, operation: &str) -> bool {
    matches!(event, Event::OperationFinished { operation: id, .. } if id.as_str() == operation)
}

/// What request `index` to the model said.
fn asked(mock: &Mock, index: usize) -> String {
    mock.requests.lock().unwrap()[index]["messages"].to_string()
}

#[tokio::test]
async fn a_message_written_while_a_call_waits_reaches_the_running_turn() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![(
        "write",
        json!({"path":"note.txt", "content":"first"}),
    )])
    .await;
    let settings = settings(project.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    let Event::Snapshot(initial) = event(&client).await else {
        panic!("Expected snapshot");
    };
    assert!(
        initial.can_steer,
        "The served agent takes messages while it works"
    );

    submit(&client, 1, "Write the note.");
    let permission = approval(&client).await;
    // The turn waits for the approval of its call: a message written now
    // goes to that turn.
    submit(&client, 2, "Mention the second ledger too.");
    let events = until(&client, |e| matches!(e, Event::MessageSteered { .. })).await;
    let decide = Command::Decide {
        permission: permission.id,
        decision: Decision::Allow,
    };
    command(&client, 3, "main", decide);
    let rest = until(&client, |e| finished(e, "operation-1")).await;
    assert!(accepted(&events, 2) || accepted(&rest, 2), "{events:?}");

    assert!(project.path().join("note.txt").exists());
    let requests = mock.requests.lock().unwrap().len();
    assert_eq!(requests, 2, "The message took no turn of its own");
    assert!(
        asked(&mock, 1).contains("Mention the second ledger too."),
        "The turn read the message at its next step"
    );
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}

/// Hold the agent's first answer as it is stored, telling `held` once it
/// waits: that turn reads no more messages.
fn hold_first_answer(app: &Application, gate: watch::Receiver<bool>, held: Arc<Notify>) {
    let first = Arc::new(AtomicUsize::new(0));
    let hold = move |event: roomkit::model::RoomEvent, _| {
        let (mut gate, held) = (gate.clone(), held.clone());
        let agent = event.source.channel_id.as_str() == "code";
        let hold = agent && first.fetch_add(1, Ordering::SeqCst) == 0;
        async move {
            if hold {
                held.notify_one();
                let _ = gate.wait_for(|open| *open).await;
            }
            Ok(roomkit::model::HookResult::allow(Default::default()))
        }
    };
    let options = HookOptions {
        name: "hold-answer".to_owned(),
        timeout: Duration::from_secs(300),
        ..Default::default()
    };
    let trigger = roomkit::HookTrigger::BeforeBroadcast;
    app.kit.hook(trigger, hold, options).unwrap();
}

/// The hook events of the turn the operation `operation` started.
fn turn_hooks(root: &std::path::Path, operation: &str) -> Vec<Value> {
    records(root)
        .into_iter()
        .filter(|e| e["roomkit"]["turn_id"] == operation)
        .collect()
}

#[tokio::test]
async fn a_message_the_turn_no_longer_reads_gets_a_turn_with_its_hooks() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let hooked = [
        "UserPromptSubmit",
        "TurnStart",
        "PreToolUse",
        "PostToolUse",
        "TurnEnd",
    ];
    configure(project.path(), &hooked, "", false);
    std::fs::write(project.path().join("source"), "sample").unwrap();
    // The first turn answers; the second reads a file, then answers.
    let mock = Mock::start(vec![("", json!({})), ("read", json!({"path":"source"}))]).await;
    let settings = settings(project.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    enable_all(&app);
    let (open, gate) = watch::channel(false);
    let held = Arc::new(Notify::new());
    hold_first_answer(&app, gate, held.clone());
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    event(&client).await;

    submit(&client, 1, "First question.");
    held.notified().await;
    submit(&client, 2, "Second question.");
    let reply =
        |e: &Event| matches!(e, Event::Reply { request, .. } if *request == RequestId::new(2));
    let events = until(&client, reply).await;
    assert!(
        accepted(&events, 2),
        "Accepted for the running turn: {events:?}"
    );
    open.send(true).unwrap();
    until(&client, |e| finished(e, "operation-1")).await;

    // The message's own turn ends after the first one: its hooks say so.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let own = loop {
        let own = turn_hooks(project.path(), "operation-2");
        if own
            .last()
            .is_some_and(|e| e["hook_event_name"] == "TurnEnd")
        {
            break own;
        }
        assert!(std::time::Instant::now() < deadline, "No TurnEnd: {own:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let names: Vec<_> = own.iter().map(|e| e["hook_event_name"].as_str()).collect();
    assert_eq!(names, hooked.map(Some), "Hooked as any turn");
    assert_eq!(own[3]["roomkit"]["outcome"], "served", "The read ran");
    assert_eq!(mock.requests.lock().unwrap().len(), 3);
    assert!(asked(&mock, 2).contains("sample"));
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}
