use super::*;
use roomkit_tools_process::{ProcessStartInput, ProcessState};
use roomkit_tui::protocol::{BackgroundStatus, PanelSpec};
use roomkit_tui_engine::{CommandHandler, CommandResult};
use tokio_util::sync::CancellationToken;

async fn panel(app: &Application, args: &str) -> std::result::Result<PanelSpec, String> {
    let result = app
        .profiles
        .processes
        .handle(&app.kit, app.channel.id(), &app.view, args)
        .await?;
    let CommandResult::Events(events) = result else {
        panic!("Expected panel events");
    };
    events
        .into_iter()
        .find_map(|e| match e {
            Event::Panel { panel, .. } => Some(panel),
            _ => None,
        })
        .ok_or("Missing panel".into())
}

fn action(panel: &PanelSpec, suffix: &str) -> String {
    panel
        .actions
        .iter()
        .find(|a| a.value.ends_with(suffix))
        .unwrap()
        .value
        .clone()
}

async fn badge(
    rx: &mut tokio::sync::watch::Receiver<Vec<Event>>,
    contains: &str,
) -> BackgroundStatus {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let found = rx.borrow_and_update().iter().find_map(|e| match e {
                Event::BackgroundChanged {
                    status: Some(s), ..
                } if s.label.contains(contains) => Some(s.clone()),
                _ => None,
            });
            if let Some(status) = found {
                return status;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

/// Wait until the published exit warning reads `expected`. The observer may
/// publish once per process start, so an earlier count can come first.
async fn exit_warning(rx: &mut tokio::sync::watch::Receiver<Vec<Event>>, expected: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let warned = rx.borrow_and_update().iter().any(|event| {
                matches!(event, Event::ExitWarning { text: Some(text), .. } if text == expected)
            });
            if warned {
                return;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

async fn no_badge(rx: &mut tokio::sync::watch::Receiver<Vec<Event>>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if rx
                .borrow_and_update()
                .iter()
                .any(|event| matches!(event, Event::BackgroundChanged { status: None, .. }))
            {
                return;
            }
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn monitor_observes_actual_lifecycle_filters_owners_and_rejects_old_actions() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: root.path().into(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
        services: app.services.clone(),
    };
    assert!(handler.allow_while_busy("processes"));
    let mut updates = handler.subscribe(app.view.clone()).unwrap();
    let manager = &app.profiles.resources.processes;
    let owner = serde_json::to_string(&(app.view.room.as_str(), app.view.agent.as_str())).unwrap();
    let cancel = CancellationToken::new();
    let process = manager
        .start(
            Some(&owner),
            ProcessStartInput::new("printf 'stdout marker'; printf 'stderr marker' >&2; cat"),
            &cancel,
        )
        .await
        .unwrap()
        .data;
    let hidden = manager
        .start(
            Some("[\"unbound-room\",\"other-agent\"]"),
            ProcessStartInput::new("cat"),
            &cancel,
        )
        .await
        .unwrap()
        .data;
    assert_eq!(
        badge(&mut updates, "running").await.label,
        "Processes: 1 running"
    );
    // Exiting stops every process, including one this view cannot see.
    exit_warning(&mut updates, "2 running processes will be stopped").await;
    let sidebar = app.profiles.sidebar(&app.view).await.0;
    let processes = sidebar
        .sections
        .iter()
        .find(|s| s.title == "Processes")
        .unwrap();
    let roomkit_tui::protocol::SidebarContent::Entries { entries } = &processes.content else {
        panic!("Expected process entries");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].action.as_ref().unwrap().arguments,
        process.process_id
    );
    assert!(
        !serde_json::to_string(&sidebar)
            .unwrap()
            .contains(&hidden.process_id)
    );
    let list = panel(&app, "").await.unwrap();
    assert_eq!(list.actions.len(), 1);
    assert!(list.body.contains(app.view.room.as_str()));
    assert!(!list.body.contains(&hidden.process_id));
    let open = action(&list, &format!("open:{}", process.process_id));
    let detail = panel(&app, &open).await.unwrap();
    assert!(detail.body.contains("Command:") && detail.body.contains("Owner: code"));
    // The directory is relative to the workspace, `.` for its root.
    assert!(detail.body.contains("\nDirectory: .\n"), "{}", detail.body);
    let stop = action(&detail, ":stop");
    assert!(
        panel(&app, &open).await.is_err(),
        "Old panel navigation must be rejected"
    );
    // A different view cannot reuse this action even though it shares the manager.
    let mut other = app.view.clone();
    other.agent = ChannelId::new("another-agent").unwrap();
    assert!(
        app.profiles
            .processes
            .handle(&app.kit, app.channel.id(), &other, &stop)
            .await
            .is_err()
    );
    let stopped = panel(&app, &stop).await.unwrap();
    assert!(stopped.title.contains("stopped"));
    assert!(stopped.body.contains("stdout marker") && stopped.body.contains("stderr marker"));
    no_badge(&mut updates).await;
    exit_warning(&mut updates, "1 running process will be stopped").await;
    let sidebar = app.profiles.sidebar(&app.view).await.0;
    let serialized = serde_json::to_string(&sidebar).unwrap();
    assert!(serialized.contains("No running processes"));
    assert!(!serialized.contains(&process.process_id));
    let jobs = manager.observe().await;
    assert_eq!(
        jobs.iter()
            .find(|p| p.status.process_id == hidden.process_id)
            .unwrap()
            .status
            .state,
        ProcessState::Running
    );
    assert!(
        panel(&app, &stop).await.is_err(),
        "Repeated stop cannot act on a fresh panel"
    );
    let list = panel(&app, &action(&stopped, ":list")).await.unwrap();
    assert!(list.actions.is_empty());
    assert!(list.body.contains("No running processes"));
    assert!(!list.body.contains(&process.process_id));
    assert!(
        panel(&app, &hidden.process_id).await.is_err(),
        "Direct output access must still enforce conversation access"
    );
    // Output stays readable by ID, without keeping the command in the live list.
    let retained = panel(&app, &process.process_id).await.unwrap();
    assert!(retained.body.contains("stdout marker"));
    let list = panel(&app, &action(&retained, ":forget")).await.unwrap();
    assert!(list.body.contains("No running processes"));
    app.profiles
        .processes
        .handle(&app.kit, app.channel.id(), &app.view, &list.dismiss)
        .await
        .unwrap();
    drop(updates);
    tokio::time::timeout(Duration::from_secs(2), async {
        while Arc::strong_count(&app.profiles.processes) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        mock.requests.lock().unwrap().is_empty(),
        "Human controls must not prompt the model"
    );
    app.close().await.unwrap();
}

#[tokio::test]
async fn monitor_pages_output_without_consuming_it_and_does_not_tick_when_closed() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    let manager = &app.profiles.resources.processes;
    let mut updates = app.profiles.processes.subscribe(
        app.kit.clone(),
        app.channel.id().clone(),
        app.view.clone(),
        app.profiles.resources.tasks.clone(),
    );
    let owner = serde_json::to_string(&(app.view.room.as_str(), app.view.agent.as_str())).unwrap();
    let p = manager.start(Some(&owner), ProcessStartInput::new("printf FIRST; i=0; while [ $i -lt 5000 ]; do printf .; i=$((i+1)); done; printf LAST; cat"), &CancellationToken::new()).await.unwrap().data;
    badge(&mut updates, "running").await;
    let list = panel(&app, "").await.unwrap();
    assert_eq!(list.actions.len(), 1);
    manager
        .write(
            Some(&owner),
            roomkit_tools_process::ProcessWriteInput {
                process_id: p.process_id.clone(),
                text: String::new(),
                close_stdin: true,
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    // The list updates itself on natural exit, without reopening it or a model turn.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if updates.borrow_and_update().iter().any(|event| {
                matches!(event,
                    Event::PanelUpdated { panel, .. } if panel.id == list.id
                        && panel.body.contains("No running processes") && panel.actions.is_empty()
                )
            }) {
                break;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    no_badge(&mut updates).await;
    let tail = panel(&app, &p.process_id).await.unwrap();
    assert!(tail.body.contains("LAST") && !tail.body.contains("\nFIRST"));
    let first = panel(&app, &action(&tail, ":first")).await.unwrap();
    assert!(first.body.contains("\nFIRST") && first.body.contains("more available"));
    let next = panel(&app, &action(&first, ":next")).await.unwrap();
    assert!(next.body.contains("LAST"));
    app.profiles
        .processes
        .handle(&app.kit, app.channel.id(), &app.view, &next.dismiss)
        .await
        .unwrap();
    // Wait until the observer acknowledges dismissal; thereafter no timer runs.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if !updates
                .borrow_and_update()
                .iter()
                .any(|event| matches!(event, Event::PanelUpdated { .. }))
            {
                break;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(1200), updates.changed())
            .await
            .is_err()
    );
    drop(updates);
    app.close().await.unwrap();
}

/// The first request the model was sent that holds `text`, as JSON text,
/// within five seconds.
async fn asked_about(mock: &Mock, text: &str) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let found = mock
                .requests
                .lock()
                .unwrap()
                .iter()
                .map(Value::to_string)
                .find(|request| request.contains(text));
            if let Some(request) = found {
                return request;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the model was never told {text:?}"))
}

/// The owner key the tools file the view's agent's jobs under.
fn view_owner(app: &Application) -> String {
    serde_json::to_string(&(app.view.room.as_str(), app.view.agent.as_str())).unwrap()
}

#[tokio::test]
async fn an_agent_is_told_when_its_background_jobs_end() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    let owner = view_owner(&app);
    let cancel = CancellationToken::new();
    let resources = &app.profiles.resources;
    resources
        .processes
        .start(
            Some(&owner),
            ProcessStartInput::new("sleep 0.2; exit 3"),
            &cancel,
        )
        .await
        .unwrap();
    asked_about(
        &mock,
        "Background process p1 (`sleep 0.2; exit 3`) exited with code 3 after",
    )
    .await;
    // What a shell command leaves in the background is followed too.
    let command = "sleep 0.3 >/dev/null 2>&1 &";
    let input = roomkit_tools_process::ShellInput::new(command);
    resources
        .shell
        .shell_owned(&owner, input, &cancel)
        .await
        .unwrap();
    asked_about(
        &mock,
        &format!("The background jobs of the shell command `{command}` ended after"),
    )
    .await;
    app.close().await.unwrap();
}

#[tokio::test]
async fn closing_the_application_stops_what_a_shell_command_left_running() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    let input =
        roomkit_tools_process::ShellInput::new("sleep 30 >/dev/null 2>&1 & echo $! > job.pid");
    let result = app
        .profiles
        .resources
        .shell
        .shell_owned(&view_owner(&app), input, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.data.exit_code, Some(0), "{}", result.text);
    let pid = std::fs::read_to_string(root.path().join("job.pid")).unwrap();
    let running = || {
        std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .status()
            .unwrap()
            .success()
    };
    assert!(running(), "the job outlives its command");
    app.close().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while running() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("closing the application did not stop the job"));
}
