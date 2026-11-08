//! An agent with a known window compacts its conversation by tiers (HITL
//! a2daedfe), and at once on `/compact`, leaving a line in the conversation
//! (HITL a2dfba34); one without keeps its latest messages.
use super::*;
use roomkit::model::{ChannelId, ChannelSet, EventType, Visibility};
use roomkit_tui::protocol::Item;
use roomkit_tui_engine::{CommandHandler, CommandResult};

/// How many words make an answer of about 4,000 estimated tokens.
const ANSWER_WORDS: usize = 2_000;

fn write(root: &std::path::Path, name: &str, text: &str) {
    std::fs::write(root.join(name), text).unwrap();
}

fn handler(app: &Application, settings: &Settings) -> crate::sessions::Sessions {
    crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: settings.workspace.clone(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
        services: app.services.clone(),
    }
}

/// Runs a turn named `operation` in a view with the application's commands
/// and live events, and returns the notices and the application's lines it
/// showed: once the turn is over, it waits up to two seconds more until
/// `done` says it saw enough.
async fn turn_noticing(
    app: &Application,
    settings: &Settings,
    operation: &str,
    done: impl Fn(&[String]) -> bool,
) -> Vec<String> {
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(
        app.session()
            .run_with_commands(host, handler(app, settings)),
    );
    let notices = tokio::time::timeout(Duration::from_secs(15), async {
        let mut session = Session::default();
        let mut notices = Vec::new();
        let (mut submitted, mut finished) = (false, None);
        loop {
            match client.try_recv() {
                Ok(event) => {
                    match &event {
                        Event::Notice { text, .. } => notices.push(text.clone()),
                        Event::Item {
                            item: Item::Message(message),
                            ..
                        } if message.actor.as_str() == crate::application::TERMINAL => {
                            notices.push(message.text.clone());
                        }
                        _ => {}
                    }
                    if let Event::OperationFinished {
                        operation: id,
                        outcome,
                        ..
                    } = &event
                        && id.as_str() == operation
                    {
                        assert_eq!(*outcome, RunOutcome::Completed);
                        finished = Some(std::time::Instant::now());
                    }
                    session.apply(event);
                    if !submitted && let Some(snapshot) = session.snapshot() {
                        let submit = Command::Submit {
                            agent: snapshot.selected_agent.clone(),
                            text: "Inspect the project.".into(),
                        };
                        let conversation = snapshot.conversation.clone();
                        let request =
                            Request::new(RequestId::new(1), operation, conversation, submit);
                        client.try_send(request).unwrap();
                        submitted = true;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Err(error) => panic!("Unexpected attachment failure: {error}"),
            }
            if let Some(at) = finished
                && (done(&notices) || at.elapsed() > Duration::from_secs(2))
            {
                break notices;
            }
        }
    })
    .await
    .unwrap();
    drop(client);
    task.await.unwrap().unwrap();
    notices
}

/// The summaries `requests` asked for: the requests not streamed.
fn summaries(requests: &[Value]) -> usize {
    requests.iter().filter(|r| r["stream"] != true).count()
}

/// An answer of about 4,000 estimated tokens, in distinct words.
fn long_answer() -> String {
    (0..ANSWER_WORDS)
        .map(|word| format!("item{word} "))
        .collect()
}

/// The conversation compacts once its requests pass 80 % of the agent's
/// window: its model summarizes the earlier turns, and the summary, with the
/// project's rules, stands for them. It then stays as it is: the turn after
/// it, even resumed, reads the same summary and extends the compacting
/// turn's request.
#[tokio::test]
async fn a_conversation_compacts_past_its_tier_and_resumes_on_its_summary() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    write(root.path(), "AGENTS.md", "ROOT_ONE");
    let mock = Mock::with_answer(vec![], &long_answer()).await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.context_window = Some(64_000);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let asked = || summaries(&mock.requests.lock().unwrap());
    let mut compacted = None;
    for turn in 1..=14 {
        let notices = turn_noticing(&app, &settings, &format!("turn-{turn}"), |notices| {
            asked() == 0
                || notices
                    .iter()
                    .any(|n| n.starts_with("Conversation compacted · "))
        })
        .await;
        if asked() > 0 {
            compacted = Some((turn, notices));
            break;
        }
    }
    let Some((turn, notices)) = compacted else {
        panic!("the conversation does not compact within 14 turns");
    };
    assert!(turn >= 3, "compacted at turn {turn}, before its tier");
    // The line reaches the view watching as it is made; the status line
    // only says "Conversation compacted".
    assert!(
        notices
            .iter()
            .any(|n| n.starts_with("Conversation compacted · ")),
        "{notices:?}"
    );
    assert!(
        notices.iter().any(|n| n == "Conversation compacted"),
        "{notices:?}"
    );
    let requests = mock.requests.lock().unwrap().clone();
    let at = requests.iter().position(|r| r["stream"] != true).unwrap();
    let compacting = requests[at + 1]["messages"].as_array().unwrap().clone();
    let opening = compacting[1]["content"].as_str().unwrap();
    assert!(
        opening.contains(SUMMARY) && opening.contains("ROOT_ONE"),
        "{opening}"
    );
    let answers = compacting.iter().filter(|m| m["role"] == "assistant");
    assert!(
        answers.count() < turn - 1,
        "the summary stands for earlier answers"
    );
    app.close().await.unwrap();
    drop(app);

    let app = Application::open(&settings, String::new()).await.unwrap();
    turn_operation(&app, "resumed").await;
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(summaries(&requests), 1, "the resumed turn compacts no more");
    let later = requests.last().unwrap();
    let messages = later["messages"].as_array().unwrap();
    assert_eq!(
        &messages[..compacting.len()],
        compacting.as_slice(),
        "the resumed turn's request extends the compacting turn's"
    );
    assert_eq!(
        later.to_string().matches("ROOT_ONE").count(),
        1,
        "the project's rules ride the summary, not the turn"
    );
    app.close().await.unwrap();
}

fn invoke(app: &Application, name: &str) -> Request {
    Request::new(
        RequestId::new(7),
        "command",
        app.view.room.as_str(),
        Command::Invoke {
            name: name.into(),
            arguments: String::new(),
        },
    )
}

fn notice(result: CommandResult) -> String {
    match result {
        CommandResult::Events(events) => events
            .into_iter()
            .find_map(|event| match event {
                Event::Notice { text, .. } => Some(text),
                _ => None,
            })
            .unwrap(),
        _ => panic!("Expected a notice"),
    }
}

/// Waits up to fifteen seconds until `done`.
async fn until(what: &str, done: impl Fn() -> bool) {
    let waited = tokio::time::timeout(Duration::from_secs(15), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "timed out waiting until {what}");
}

/// The requests a turn streamed, not a summary's.
fn streamed(requests: &[Value]) -> usize {
    requests.len() - summaries(requests)
}

/// An application on `mock` whose agent's window is known, after three
/// turns.
async fn three_turns(
    root: &std::path::Path,
    state: &std::path::Path,
    mock: &Mock,
) -> (Application, Settings) {
    let mut settings = settings(root, state, &mock.base_url);
    settings.context_window = Some(200_000);
    let app = Application::open(&settings, String::new()).await.unwrap();
    for turn in ["first", "second", "third"] {
        turn_operation(&app, turn).await;
    }
    (app, settings)
}

/// `/compact` compacts at once, with no message to wait for, and leaves its
/// line in the conversation: stored for the terminal alone, it is shown in
/// its place once the conversation is reloaded, and the agent never reads it.
#[tokio::test]
async fn compact_starts_at_once_and_leaves_its_line_in_the_conversation() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mock = Mock::start(vec![]).await;
    let (app, settings) = three_turns(root.path(), state.path(), &mock).await;
    let asked = handler(&app, &settings)
        .handle(invoke(&app, "compact"), app.view.clone())
        .await
        .unwrap();
    assert_eq!(notice(asked), "Compacting the conversation…");
    let room = app.view.room.to_string();
    let compactions = app.profiles.resources.compactions.clone();
    // Its line is written last.
    until("the conversation compacts", || {
        compactions.lines(&room).is_some()
    })
    .await;
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(
        (streamed(&requests), summaries(&requests)),
        (3, 1),
        "the summary is asked with no message sent"
    );
    let options = roomkit::GetTimelineOptions {
        limit: 1_000,
        ..Default::default()
    };
    let rows = app.kit.get_timeline(&app.view.room, options).await.unwrap();
    let stored: Vec<_> = rows
        .iter()
        .filter(|row| row.r#type == EventType::System)
        .collect();
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(
        stored[0].source.channel_id.as_str(),
        crate::application::TERMINAL
    );
    let terminal = ChannelId::new(crate::application::TERMINAL).unwrap();
    let only = ChannelSet::new([terminal])
        .map(Visibility::Channels)
        .unwrap();
    assert_eq!(stored[0].visibility, only, "the terminal's alone");
    let line = roomkit_core::rules::transcoding::text_of(&stored[0].content);
    assert!(
        line.starts_with("Conversation compacted · 2 messages summarized · ≈"),
        "the first turn's: four messages at least are kept whole: {line}"
    );

    let session = turn_operation(&app, "fourth").await;
    let texts: Vec<&str> = session
        .snapshot()
        .unwrap()
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Message(message) => Some(message.text.as_str()),
            _ => None,
        })
        .collect();
    let at = texts.iter().position(|text| *text == line).unwrap();
    assert_eq!(
        &texts[at - 1..],
        [
            "Project inspection complete.",
            line.as_str(),
            "Inspect the project.",
            "Project inspection complete."
        ],
        "the line stands between the third turn and the fourth"
    );
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(summaries(&requests), 1, "the next turn compacts no more");
    let fourth = requests.last().unwrap();
    let opening = fourth["messages"][1]["content"].as_str().unwrap();
    assert!(opening.contains(SUMMARY), "{opening}");
    let asked = fourth.to_string().matches("Inspect the project.").count();
    assert_eq!(asked, 3, "the summary stands for the first turn");
    assert!(
        !fourth.to_string().contains("Conversation compacted"),
        "the agent never reads the line"
    );
    app.close().await.unwrap();
}

/// A compaction outside the turns summarizes what a turn's would: the
/// messages, not the agent's tool calls nor the instructions its turns read.
#[tokio::test]
async fn compact_summarizes_the_messages_alone() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(root.path(), "AGENTS.md", "PROJECT_RULES_TEXT");
    let mock = Mock::start(vec![("list", json!({"path":"."}))]).await;
    let (app, settings) = three_turns(root.path(), state.path(), &mock).await;
    let called = mock.requests.lock().unwrap()[1].to_string();
    assert!(called.contains("PROJECT_RULES_TEXT") && called.contains("tool_calls"));
    handler(&app, &settings)
        .handle(invoke(&app, "compact"), app.view.clone())
        .await
        .unwrap();
    let room = app.view.room.to_string();
    let compactions = app.profiles.resources.compactions.clone();
    until("the conversation compacts", || {
        compactions.lines(&room).is_some()
    })
    .await;
    let requests = mock.requests.lock().unwrap().clone();
    let asked = requests
        .iter()
        .find(|r| r["stream"] != true)
        .unwrap()
        .to_string();
    assert!(asked.contains("Inspect the project."), "{asked}");
    for read in ["tool_call", "Application mode", "PROJECT_RULES_TEXT"] {
        assert!(!asked.contains(read), "the summary read {read}: {asked}");
    }
    app.close().await.unwrap();
}

/// A size on a line, `≈19.9k` read as 19,900 tokens.
fn tokens(size: &str) -> usize {
    match size.strip_suffix('k') {
        Some(thousands) => (thousands.parse::<f64>().unwrap() * 1_000.0) as usize,
        None => size.parse().unwrap(),
    }
}

/// The size after a line's arrow.
fn after(line: &str) -> usize {
    tokens(line.split("→ ≈").nth(1).unwrap().split(' ').next().unwrap())
}

/// The size before a line's arrow.
fn before(line: &str) -> usize {
    tokens(
        line.split(" · ≈")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap(),
    )
}

/// Outside a turn, the sizes count what a turn replays of the agent's tool
/// calls: the first turn's read, which the summary stands for, is in the
/// size before the compaction, not after.
#[tokio::test]
async fn a_compaction_s_sizes_count_the_rounds_a_turn_replays() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let large: String = (0..700)
        .map(|line| format!("line {line} of notes\n"))
        .collect();
    write(root.path(), "notes.md", &large);
    let mock = Mock::start(vec![("read", json!({"path":"notes.md"}))]).await;
    let (app, settings) = three_turns(root.path(), state.path(), &mock).await;
    // Within the engine's bound, the round replays the read whole, about
    // 3,900 tokens, as its stored row holds it.
    let read = mock.requests.lock().unwrap()[1].to_string();
    assert!(
        read.contains("line 1 of notes"),
        "the round replays the read"
    );
    handler(&app, &settings)
        .handle(invoke(&app, "compact"), app.view.clone())
        .await
        .unwrap();
    let room = app.view.room.to_string();
    let compactions = app.profiles.resources.compactions.clone();
    until("the conversation compacts", || {
        compactions.lines(&room).is_some()
    })
    .await;
    let Some(Item::Message(line)) = compactions.lines(&room) else {
        panic!("no line");
    };
    let saved = before(&line.text).saturating_sub(after(&line.text));
    assert!(saved > 1_500, "the read's tokens were saved: {}", line.text);
    app.close().await.unwrap();
}

/// Resumed, no turn measured yet what a request takes besides its history:
/// the line reads it from the latest measured request, the system prompt
/// and the tools in the size after the compaction.
#[tokio::test]
async fn a_resumed_conversation_s_line_counts_the_rest_of_the_request() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let measured = json!({"prompt_tokens": 20_000, "completion_tokens": 10});
    let reports = vec![Some(measured); 3];
    let mock = Mock::with_reports(vec![], "Project inspection complete.", reports).await;
    let (app, settings) = three_turns(root.path(), state.path(), &mock).await;
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    handler(&app, &settings)
        .handle(invoke(&app, "compact"), app.view.clone())
        .await
        .unwrap();
    let room = app.view.room.to_string();
    let compactions = app.profiles.resources.compactions.clone();
    until("the conversation compacts", || {
        compactions.lines(&room).is_some()
    })
    .await;
    let Some(Item::Message(line)) = compactions.lines(&room) else {
        panic!("no line");
    };
    assert!(after(&line.text) > 15_000, "{}", line.text);
    app.close().await.unwrap();
}

/// Resumed, the rest of the request read from the latest measured one leaves
/// out what it replayed of the agent's rounds: the first turn's read, which
/// the summary stands for, is in no size after the compaction.
#[tokio::test]
async fn a_resumed_conversation_s_line_leaves_out_the_summarized_rounds() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let large: String = (0..700)
        .map(|line| format!("line {line} of notes\n"))
        .collect();
    write(root.path(), "notes.md", &large);
    let measured = json!({"prompt_tokens": 20_000, "completion_tokens": 10});
    let calls = vec![("read", json!({"path":"notes.md"}))];
    let mock = Mock::with_reports(
        calls,
        "Project inspection complete.",
        vec![Some(measured); 4],
    )
    .await;
    let (app, settings) = three_turns(root.path(), state.path(), &mock).await;
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    handler(&app, &settings)
        .handle(invoke(&app, "compact"), app.view.clone())
        .await
        .unwrap();
    let room = app.view.room.to_string();
    let compactions = app.profiles.resources.compactions.clone();
    until("the conversation compacts", || {
        compactions.lines(&room).is_some()
    })
    .await;
    let Some(Item::Message(line)) = compactions.lines(&room) else {
        panic!("no line");
    };
    // 20,000 measured, less the read of about 3,900 tokens.
    let after = after(&line.text);
    assert!((15_000..=18_500).contains(&after), "{}", line.text);
    app.close().await.unwrap();
}

/// `/compact` sent while a turn runs waits for it: it is queued, the view
/// says so, then it runs as the turn ends, with no message to wait for.
#[tokio::test]
async fn compact_during_a_turn_runs_once_the_turn_ends() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mock = Mock::start(vec![]).await;
    let (app, settings) = three_turns(root.path(), state.path(), &mock).await;
    assert!(handler(&app, &settings).allow_while_busy("compact"));
    let (room, agent) = (app.view.room.to_string(), app.view.agent.to_string());
    let compactions = app.profiles.resources.compactions.clone();
    let mut held = Some(mock.turns_held.lock().await);
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(
        app.session()
            .run_with_commands(host, handler(&app, &settings)),
    );
    let mut notices = Vec::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut session = Session::default();
        let (mut submitted, mut invoked) = (false, false);
        loop {
            let event = match client.try_recv() {
                Ok(event) => event,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // The turn's request waits at the server: the turn runs.
                    if submitted && !invoked && streamed(&mock.requests.lock().unwrap()) == 4 {
                        let compact = Command::Invoke {
                            name: "compact".into(),
                            arguments: String::new(),
                        };
                        let request =
                            Request::new(RequestId::new(2), "compact", room.as_str(), compact);
                        client.try_send(request).unwrap();
                        invoked = true;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    continue;
                }
                Err(error) => panic!("Unexpected attachment failure: {error}"),
            };
            match &event {
                Event::Notice { text, .. } => notices.push(text.clone()),
                Event::Reply { request, error } if request.get() == 2 => {
                    assert_eq!(*error, None, "allowed during the turn");
                    assert_eq!(summaries(&mock.requests.lock().unwrap()), 0);
                    assert_eq!(
                        compactions.notices(&room),
                        ["Compaction queued: it runs when the agent's turn ends"]
                    );
                    held = None;
                }
                Event::OperationFinished {
                    operation, outcome, ..
                } if operation.as_str() == "fourth" => {
                    assert_eq!(*outcome, RunOutcome::Completed);
                    break;
                }
                _ => {}
            }
            session.apply(event);
            if !submitted && let Some(snapshot) = session.snapshot() {
                let submit = Command::Submit {
                    agent: snapshot.selected_agent.clone(),
                    text: "Inspect the project.".into(),
                };
                let conversation = snapshot.conversation.clone();
                client
                    .try_send(Request::new(
                        RequestId::new(1),
                        "fourth",
                        conversation,
                        submit,
                    ))
                    .unwrap();
                submitted = true;
            }
        }
    })
    .await
    .unwrap();
    assert!(held.is_none());
    assert!(
        notices.contains(&"Compaction queued: it runs when the agent's turn ends".to_owned()),
        "{notices:?}"
    );
    until("the conversation compacts", || {
        compactions.get(&room, &agent).is_some()
    })
    .await;
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!((streamed(&requests), summaries(&requests)), (4, 1));
    let fourth = requests.iter().rfind(|r| r["stream"] == true).unwrap();
    assert!(
        !fourth.to_string().contains(SUMMARY),
        "the turn read its history whole"
    );
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}

/// A message sent while a compaction runs waits for it, the status line
/// counting its seconds, then reads what it made.
#[tokio::test]
async fn a_message_sent_during_a_compaction_reads_what_it_made() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mock = Mock::start(vec![]).await;
    let (app, settings) = three_turns(root.path(), state.path(), &mock).await;
    let held = mock.summaries_held.lock().await;
    handler(&app, &settings)
        .handle(invoke(&app, "compact"), app.view.clone())
        .await
        .unwrap();
    let requests = &mock.requests;
    until("the summary is asked", || {
        summaries(&requests.lock().unwrap()) == 1
    })
    .await;
    let room = app.view.room.to_string();
    let compactions = app.profiles.resources.compactions.clone();
    let status = compactions.notices(&room);
    assert!(
        status[0].starts_with("Compacting the conversation… "),
        "{status:?}"
    );
    let asked = handler(&app, &settings)
        .handle(invoke(&app, "compact"), app.view.clone())
        .await
        .unwrap();
    assert_eq!(
        notice(asked),
        "A compaction is already running in this conversation"
    );
    let ((), _) = tokio::join!(
        async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let streamed = streamed(&requests.lock().unwrap());
            assert_eq!(streamed, 3, "the turn waits for the compaction");
            drop(held);
        },
        turn_operation(&app, "fourth")
    );
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!((streamed(&requests), summaries(&requests)), (4, 1));
    let opening = requests.last().unwrap()["messages"][1]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(opening.contains(SUMMARY), "{opening}");
    assert!(
        compactions
            .notices(&room)
            .iter()
            .all(|n| !n.starts_with("Compacting"))
    );
    app.close().await.unwrap();
}

/// An agent without a known window keeps its latest messages, and its turns
/// carry the project's rules every time; with one, the turns after
/// `/compact` read its summary, and rules that changed since once.
#[tokio::test]
async fn a_window_of_messages_keeps_the_rules_each_turn_and_compacted_rules_go_once() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    write(root.path(), "AGENTS.md", "ROOT_ONE");
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let refused = handler(&app, &settings)
        .handle(invoke(&app, "compact"), app.view.clone())
        .await
        .unwrap();
    assert!(notice(refused).contains("latest 50 messages"));
    for turn in ["first", "second", "third"] {
        turn_operation(&app, turn).await;
    }
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(summaries(&requests), 0);
    let last = requests.last().unwrap().to_string();
    assert_eq!(
        last.matches("ROOT_ONE").count(),
        3,
        "each turn carries the rules"
    );
    app.close().await.unwrap();
    drop(app);

    // A new project state: its agent is created with a known window.
    let state = tempfile::tempdir().unwrap();
    let before = mock.requests.lock().unwrap().len();
    let (app, settings) = three_turns(root.path(), state.path(), &mock).await;
    handler(&app, &settings)
        .handle(invoke(&app, "compact"), app.view.clone())
        .await
        .unwrap();
    let (room, agent) = (app.view.room.to_string(), app.view.agent.to_string());
    let compactions = app.profiles.resources.compactions.clone();
    until("the conversation compacts", || {
        compactions.get(&room, &agent).is_some()
    })
    .await;
    turn_operation(&app, "fourth").await;
    turn_operation(&app, "fifth").await;
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(summaries(&requests[before..]), 1, "compacted once");
    // Rules changed since: the next turn sends them, once; the one after
    // reads them where that turn did.
    write(root.path(), "AGENTS.md", "ROOT_TWO");
    turn_operation(&app, "sixth").await;
    let sixth = mock.requests.lock().unwrap().last().unwrap().to_string();
    assert_eq!(sixth.matches("ROOT_TWO").count(), 1, "{sixth}");
    turn_operation(&app, "seventh").await;
    let seventh = mock.requests.lock().unwrap().last().unwrap().to_string();
    assert_eq!(
        seventh.matches("ROOT_TWO").count(),
        1,
        "sent once, then replayed"
    );
    app.close().await.unwrap();
}

/// A summary the model does not write compacts nothing: the conversation is
/// read as it is, the view says so, and the next turn tries again.
#[tokio::test]
async fn a_failed_summary_compacts_nothing_and_is_tried_again() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mock = Mock::summarizing("An answer.", "").await;
    let (app, settings) = three_turns(root.path(), state.path(), &mock).await;
    handler(&app, &settings)
        .handle(invoke(&app, "compact"), app.view.clone())
        .await
        .unwrap();
    let (room, agent) = (app.view.room.to_string(), app.view.agent.to_string());
    let compactions = app.profiles.resources.compactions.clone();
    let failed = || {
        compactions
            .notices(&room)
            .first()
            .is_some_and(|n| n.starts_with("Compaction failed"))
    };
    until("the compaction fails", failed).await;
    assert_eq!(summaries(&mock.requests.lock().unwrap()), 1);
    assert_eq!(compactions.get(&room, &agent), None, "nothing is compacted");
    turn_operation(&app, "fourth").await;
    assert_eq!(
        summaries(&mock.requests.lock().unwrap()),
        2,
        "the turn tries again"
    );
    assert_eq!(compactions.get(&room, &agent), None);
    *mock.summary.lock().unwrap() = SUMMARY.into();
    turn_operation(&app, "fifth").await;
    assert_eq!(
        summaries(&mock.requests.lock().unwrap()),
        3,
        "and the next one"
    );
    assert_eq!(compactions.get(&room, &agent).unwrap().summary, SUMMARY);
    app.close().await.unwrap();
}
