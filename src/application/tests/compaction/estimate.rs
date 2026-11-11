//! A compaction tells its views what it left, which their gauge shows until
//! a turn of the agent tells better (roomkit-tui RMK-1646).
use super::*;
use roomkit_tui::protocol::ContextUsage;

/// What a view attached now shows as the conversation's context, once it
/// read the application's live state, whose sidebar follows an estimate.
async fn shown(app: &Application, settings: &Settings) -> Option<ContextUsage> {
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(
        app.session()
            .run_with_commands(host, handler(app, settings)),
    );
    let shown = tokio::time::timeout(Duration::from_secs(15), async {
        let mut session = Session::default();
        loop {
            match client.try_recv() {
                Ok(event) => {
                    let live = matches!(event, Event::SidebarChanged { .. });
                    session.apply(event);
                    if live {
                        break session.context().cloned();
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Err(error) => panic!("Unexpected attachment failure: {error}"),
            }
        }
    })
    .await
    .unwrap();
    drop(client);
    task.await.unwrap().unwrap();
    shown
}

/// What a view shows: the tokens, and whether they are an estimate.
fn reads(usage: Option<ContextUsage>) -> (u64, bool) {
    let usage = usage.unwrap();
    (usage.tokens, usage.estimated)
}

/// A server whose three first turns measure 20,010 tokens each, and the turn
/// after the summary 9,010.
async fn measuring() -> Mock {
    let turn = json!({"prompt_tokens": 20_000, "completion_tokens": 10});
    let next = json!({"prompt_tokens": 9_000, "completion_tokens": 10});
    let reports = vec![
        Some(turn.clone()),
        Some(turn.clone()),
        Some(turn),
        None,
        Some(next),
    ];
    Mock::with_reports(vec![], "Project inspection complete.", reports).await
}

/// `/compact`, until its line is written; the line.
async fn compact(app: &Application, settings: &Settings) -> String {
    handler(app, settings)
        .handle(invoke(app, "compact"), app.view.clone())
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
    line.text
}

/// The estimate the conversation's compaction keeps.
fn kept(app: &Application) -> Option<crate::compaction::Estimate> {
    let compactions = &app.profiles.resources.compactions;
    let compacted = compactions.get(app.view.room.as_str(), app.view.agent.as_str());
    compacted.and_then(|compacted| compacted.estimate)
}

/// Found in use: after `/compact`, the line said ≈92k tokens and the gauge
/// kept the 651k measured before, until the next turn's first answer.
#[tokio::test]
async fn a_compaction_between_turns_tells_its_views_what_it_left() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mock = measuring().await;
    let (app, settings) = three_turns(root.path(), state.path(), &mock).await;
    assert_eq!(reads(shown(&app, &settings).await), (20_010, false));

    let line = compact(&app, &settings).await;
    let told = shown(&app, &settings).await.unwrap();
    assert!(told.estimated, "{told:?}");
    assert_eq!(told.capacity, Some(200_000), "the agent's window");
    let size = crate::compaction::tokens(usize::try_from(told.tokens).unwrap());
    assert!(
        line.contains(&format!("→ ≈{size} tokens")),
        "the size its line tells: {line}"
    );

    // Relaunched before any turn, a view shows it again.
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(shown(&app, &settings).await, Some(told));

    // The next turn measures the conversation: its end forgets the estimate,
    // with no view attached to ask for it.
    turn_operation(&app, "fourth").await;
    until("the turn's end forgets the estimate", || {
        kept(&app).is_none()
    })
    .await;
    assert_eq!(reads(shown(&app, &settings).await), (9_010, false));
    // Relaunched, a view reads the turn's measure, and no estimate over it.
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(reads(shown(&app, &settings).await), (9_010, false));
    app.close().await.unwrap();
}

/// A launch that ended before it forgot an estimate leaves it on disk: the
/// next one reads that the agent's turn came after it, and shows the turn's
/// measure.
#[tokio::test]
async fn an_estimate_a_turn_outdated_is_not_shown_by_a_later_launch() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mock = measuring().await;
    let (app, settings) = three_turns(root.path(), state.path(), &mock).await;
    compact(&app, &settings).await;
    let estimate = kept(&app).unwrap();
    turn_operation(&app, "fourth").await;
    until("the turn's end forgets the estimate", || {
        kept(&app).is_none()
    })
    .await;
    let agent = app.view.agent.to_string();
    let records = app._storage.database.with_file_name("compactions");
    app.close().await.unwrap();
    drop(app);

    // The record, as that launch would have left it.
    let files: Vec<_> = std::fs::read_dir(&records).unwrap().collect();
    assert_eq!(files.len(), 1, "one conversation compacted");
    let file = files[0].as_ref().unwrap().path();
    let mut record: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert!(record["agents"][&agent]["estimate"].is_null());
    record["agents"][&agent]["estimate"] = serde_json::to_value(&estimate).unwrap();
    std::fs::write(&file, record.to_string()).unwrap();

    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(kept(&app), Some(estimate), "read back");
    assert_eq!(reads(shown(&app, &settings).await), (9_010, false));
    assert_eq!(kept(&app), None, "and forgotten for good");
    app.close().await.unwrap();
}

/// A compaction a turn makes as it starts is followed by the turn's own
/// requests: its estimate goes with the turn's end.
#[tokio::test]
async fn a_compaction_a_turn_makes_leaves_no_estimate_once_the_turn_ends() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mock = measuring().await;
    let (app, settings) = three_turns(root.path(), state.path(), &mock).await;
    let compactions = app.profiles.resources.compactions.clone();
    let (room, agent) = (app.view.room.to_string(), app.view.agent.to_string());
    assert!(compactions.force(&room, &agent), "the next turn compacts");
    turn_operation(&app, "fourth").await;
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(summaries(&requests), 1, "the turn compacted");
    until("the turn's end forgets the estimate", || {
        compactions
            .get(&room, &agent)
            .is_some_and(|compacted| compacted.estimate.is_none())
    })
    .await;
    assert_eq!(reads(shown(&app, &settings).await), (9_010, false));
    app.close().await.unwrap();
}

/// Runs a turn named `operation` and cuts it once `ready`, as Esc does.
async fn cut_turn(app: &Application, operation: &str, ready: impl Fn() -> bool) {
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut session = Session::default();
        let (mut submitted, mut cut, mut run) = (false, false, None);
        loop {
            let event = match client.try_recv() {
                Ok(event) => event,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    if let (false, true, Some(run)) = (cut, ready(), run.clone()) {
                        let stop = Command::Interrupt { run };
                        let room = app.view.room.as_str();
                        let request = Request::new(RequestId::new(2), "cut", room, stop);
                        client.try_send(request).unwrap();
                        cut = true;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    continue;
                }
                Err(error) => panic!("Unexpected attachment failure: {error}"),
            };
            match &event {
                Event::RunStarted { run: started, .. } => run = Some(started.clone()),
                Event::OperationFinished {
                    operation: id,
                    outcome,
                    ..
                } if id.as_str() == operation => {
                    assert_eq!(*outcome, RunOutcome::Interrupted);
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
                let request = Request::new(RequestId::new(1), operation, conversation, submit);
                client.try_send(request).unwrap();
                submitted = true;
            }
        }
    })
    .await
    .unwrap();
    drop(client);
    task.await.unwrap().unwrap();
}

/// Found by reading: a turn cut in a tool leaves its record on the call's
/// row, after the conversation's last message. It came before the
/// compaction, whose estimate stands.
#[tokio::test]
async fn a_compaction_after_a_turn_cut_in_a_tool_tells_what_it_left() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let turn = json!({"prompt_tokens": 20_000, "completion_tokens": 10});
    let calling = json!({"prompt_tokens": 21_000, "completion_tokens": 10});
    let reports = vec![
        Some(turn.clone()),
        Some(turn.clone()),
        Some(turn),
        Some(calling),
    ];
    let slow = json!({"command": "printf made > made.txt; sleep 30"});
    let answer = ("", json!(null));
    let calls = vec![answer.clone(), answer.clone(), answer, ("shell", slow)];
    let mock = Mock::with_reports(calls, "Project inspection complete.", reports).await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.context_window = Some(200_000);
    settings.mode = crate::permissions::Mode::Yolo;
    let app = Application::open(&settings, String::new()).await.unwrap();
    for turn in ["first", "second", "third"] {
        turn_operation(&app, turn).await;
    }
    // The fourth turn's request is measured, then its command is cut.
    let made = root.path().join("made.txt");
    cut_turn(&app, "fourth", || made.exists()).await;
    assert_eq!(reads(shown(&app, &settings).await), (21_010, false));

    let line = compact(&app, &settings).await;
    let told = shown(&app, &settings).await.unwrap();
    assert!(told.estimated && told.tokens != 21_010, "{told:?}: {line}");
    assert!(kept(&app).is_some(), "no turn ended since");
    app.close().await.unwrap();
}

/// A provider that reports no usage measures nothing: the estimate, which
/// tells the conversation before the turn, goes with the turn all the same.
#[tokio::test]
async fn an_estimate_goes_with_a_turn_that_measured_nothing() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mock = Mock::start(vec![]).await;
    let (app, settings) = three_turns(root.path(), state.path(), &mock).await;
    assert_eq!(shown(&app, &settings).await, None, "nothing measured");
    compact(&app, &settings).await;
    let told = shown(&app, &settings).await.unwrap();
    assert!(told.estimated, "{told:?}");

    turn_operation(&app, "fourth").await;
    until("the turn's end forgets the estimate", || {
        kept(&app).is_none()
    })
    .await;
    assert_eq!(shown(&app, &settings).await, None);
    app.close().await.unwrap();
}

/// A turn cut before its first answer leaves rows of the agent's, and no
/// record: nothing told better than the estimate, which a view attached
/// after a relaunch still shows.
#[tokio::test]
async fn an_estimate_stands_after_a_turn_cut_before_it_answered() {
    let (root, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mock = measuring().await;
    let (app, settings) = three_turns(root.path(), state.path(), &mock).await;
    compact(&app, &settings).await;
    let told = shown(&app, &settings).await.unwrap();
    assert!(told.estimated, "{told:?}");

    let held = mock.turns_held.lock().await;
    let before = streamed(&mock.requests.lock().unwrap());
    let waits = || streamed(&mock.requests.lock().unwrap()) > before;
    cut_turn(&app, "fourth", waits).await;
    drop(held);
    let options = roomkit::GetTimelineOptions {
        limit: 1_000,
        ..Default::default()
    };
    let rows = app.kit.get_timeline(&app.view.room, options).await.unwrap();
    let after = kept(&app).unwrap().after;
    assert!(
        rows.iter()
            .any(|row| row.index > after && row.source.channel_id == app.view.agent),
        "the cut turn left a row of the agent's"
    );
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(shown(&app, &settings).await, Some(told));
    app.close().await.unwrap();
}
