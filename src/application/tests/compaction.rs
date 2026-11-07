//! An agent with a known window compacts its conversation by tiers (HITL
//! a2daedfe); one without keeps its latest messages.
use super::*;
use roomkit_tui_engine::CommandResult;

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
/// and live events, and returns the notices it showed: once the turn is over,
/// it waits up to two seconds more until `done` says it saw enough.
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
                    if let Event::Notice { text, .. } = &event {
                        notices.push(text.clone());
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
            asked() == 0 || notices.iter().any(|n| n.contains("compacted"))
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
    assert!(
        notices
            .iter()
            .any(|n| n.starts_with("Conversation compacted: a summary stands for")),
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

/// `/compact` makes the next turn compact whatever its tier; an agent without
/// a known window keeps its latest messages, and its turns carry the
/// project's rules every time.
#[tokio::test]
async fn compact_compacts_at_the_next_turn_and_a_window_of_messages_keeps_the_rules_each_turn() {
    use roomkit_tui_engine::CommandHandler;
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
    let mut settings = super::settings(root.path(), state.path(), &mock.base_url);
    settings.context_window = Some(200_000);
    let app = Application::open(&settings, String::new()).await.unwrap();
    for turn in ["first", "second", "third"] {
        turn_operation(&app, turn).await;
    }
    let asked = handler(&app, &settings)
        .handle(invoke(&app, "compact"), app.view.clone())
        .await
        .unwrap();
    assert_eq!(
        notice(asked),
        "The conversation compacts at your next message"
    );
    let before = mock.requests.lock().unwrap().len();
    turn_operation(&app, "fourth").await;
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(summaries(&requests[before..]), 1, "the next turn compacts");
    turn_operation(&app, "fifth").await;
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(summaries(&requests[before..]), 1, "and only that one");
    app.close().await.unwrap();
}
