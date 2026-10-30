use super::*;
use roomkit_tui::{Client, protocol::RunId};

async fn attach(
    app: &Application,
    workspace: &std::path::Path,
) -> (
    Client,
    tokio::task::JoinHandle<std::result::Result<(), roomkit_cli_roomkit::AdapterError>>,
) {
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: workspace.to_path_buf(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
    };
    let task = tokio::spawn(app.session().run_with_commands(host, handler));
    assert!(matches!(next(&client).await, Event::Snapshot(_)));
    (client, task)
}

async fn next(client: &Client) -> Event {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match client.try_recv() {
                Ok(event) => return event,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(2)).await
                }
                Err(error) => panic!("Attachment failed: {error}"),
            }
        }
    })
    .await
    .unwrap()
}

fn command(app: &Application, operation: &str, text: &str) -> Request {
    Request {
        id: RequestId(30),
        operation: operation.into(),
        conversation: app.view.room.as_str().into(),
        command: Command::Invoke {
            name: "shell".into(),
            arguments: text.into(),
        },
    }
}

async fn finish(client: &Client) -> (Vec<Event>, RunOutcome) {
    let mut events = Vec::new();
    loop {
        let event = next(client).await;
        assert!(
            !matches!(&event, Event::Reply { error: Some(e), .. } if !e.is_empty()),
            "{event:?}"
        );
        assert!(
            !matches!(event, Event::PermissionRequested { .. }),
            "Explicit user commands need no agent approval"
        );
        if let Event::RunFinished { outcome, .. } = event {
            return (events, outcome);
        }
        events.push(event);
    }
}

fn body(events: &[Event]) -> &str {
    events
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::Item {
                item: Item::Message(m),
                ..
            } => Some(m.text.as_str()),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn user_shell_persists_output_and_next_model_turn_sees_it_without_automatic_generation() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, task) = attach(&app, root.path()).await;
    let text = "printf x >> runs.txt\nprintf 'quoted %s\\n' 'two words'\nprintf 'stderr marker\\n' >&2\npwd\nexit 7\n";
    client.try_send(command(&app, "manual-one", text)).unwrap();
    let (events, outcome) = finish(&client).await;
    assert_eq!(outcome, RunOutcome::Failed);
    let output = body(&events).to_string();
    assert!(output.contains("quoted two words"));
    assert!(output.contains("stderr marker"));
    assert!(output.contains("exit 7"));
    assert!(output.contains(root.path().to_str().unwrap()));
    assert!(mock.requests.lock().unwrap().is_empty());
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
    drop(app);

    let app = Application::open(&settings, String::new()).await.unwrap();
    let restored = app.session().snapshot().await.unwrap();
    assert_eq!(
        restored
            .items
            .iter()
            .filter(|item| matches!(item, Item::Message(m) if m.text == output))
            .count(),
        1
    );
    let (client, task) = attach(&app, root.path()).await;
    client.try_send(command(&app, "manual-one", text)).unwrap();
    loop {
        if let Event::Item {
            item: Item::Message(m),
            ..
        } = next(&client).await
        {
            assert_eq!(m.text, output);
            break;
        }
    }
    client
        .try_send(command(&app, "manual-one", "printf bad >> runs.txt"))
        .unwrap();
    loop {
        if let Event::Reply {
            error: Some(error), ..
        } = next(&client).await
        {
            assert!(error.contains("different command"));
            break;
        }
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join("runs.txt")).unwrap(),
        "x"
    );
    drop(client);
    task.await.unwrap().unwrap();
    turn(&app).await;
    {
        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let messages = requests[0]["messages"].as_array().unwrap();
        assert!(
            messages.iter().any(|message| message["role"] == "user"
                && message.to_string().contains("quoted two words"))
        );
    }
    app.close().await.unwrap();
}

#[tokio::test]
async fn manual_shell_is_interruptible_visible_in_processes_and_shutdown_joins_it() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut settings = settings(root.path(), state.path(), "http://127.0.0.1:1/v1");
    settings.base_url.clear(); // Explicit shell also works before any agent is configured.
    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, task) = attach(&app, root.path()).await;
    client
        .try_send(command(
            &app,
            "long",
            "printf 'started\\n'; sleep 30; printf unwanted > late.txt",
        ))
        .unwrap();
    let mut run: Option<RunId> = None;
    loop {
        match next(&client).await {
            Event::RunStarted { run: id, .. } => run = Some(id),
            Event::Item {
                item: Item::Message(m),
                ..
            } if m.text.contains("**stdout**") => break,
            _ => {}
        }
    }
    assert!(
        app.profiles
            .resources
            .processes
            .observe()
            .await
            .iter()
            .any(|p| p.status.state == roomkit_tools_process::ProcessState::Running)
    );
    let mut request = command(&app, "interrupt", "");
    request.command = Command::Interrupt { run: run.unwrap() };
    client.try_send(request).unwrap();
    let (events, outcome) = finish(&client).await;
    assert_eq!(outcome, RunOutcome::Interrupted);
    assert!(body(&events).contains("Interrupted"));
    assert!(!root.path().join("late.txt").exists());
    assert!(
        app.profiles
            .resources
            .processes
            .observe()
            .await
            .iter()
            .all(|p| p.status.state == roomkit_tools_process::ProcessState::Finished)
    );
    client
        .try_send(command(
            &app,
            "after-interrupt",
            "cat; printf 'after EOF\\n'",
        ))
        .unwrap();
    let (events, outcome) = finish(&client).await;
    assert_eq!(outcome, RunOutcome::Completed);
    assert!(body(&events).contains("after EOF"));

    client
        .try_send(command(
            &app,
            "on-close",
            "sleep 30; printf unwanted > late.txt",
        ))
        .unwrap();
    while !matches!(next(&client).await, Event::Item { .. }) {}
    drop(client);
    task.await.unwrap().unwrap();
    tokio::time::timeout(Duration::from_secs(5), app.close())
        .await
        .unwrap()
        .unwrap();
    assert!(!root.path().join("late.txt").exists());
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert!(app.session().snapshot().await.unwrap().items.iter().any(|item| matches!(item, Item::Message(m) if m.text.contains("Interrupted") && m.text.contains("sleep 30; printf unwanted"))));
    app.close().await.unwrap();
}

#[tokio::test]
async fn repeated_manual_commands_reclaim_completed_slots_and_bound_output() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let app = Application::open(
        &settings(root.path(), state.path(), "http://127.0.0.1:1/v1"),
        String::new(),
    )
    .await
    .unwrap();
    let (client, task) = attach(&app, root.path()).await;
    for index in 0..10 {
        client
            .try_send(command(&app, &format!("repeat-{index}"), "printf ok"))
            .unwrap();
        assert_eq!(finish(&client).await.1, RunOutcome::Completed);
    }
    client
        .try_send(command(
            &app,
            "bounded",
            "head -c 20000 /dev/zero | tr '\\000' x",
        ))
        .unwrap();
    let (events, outcome) = finish(&client).await;
    assert_eq!(outcome, RunOutcome::Completed);
    assert!(body(&events).contains("Output shortened"));
    assert!(body(&events).len() < 5000);
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}
