use super::*;
use roomkit_cli_roomkit::{CommandHandler, CommandResult};
use roomkit_tools_process::{ProcessStartInput, ProcessState};
use roomkit_tui::protocol::{BackgroundStatus, PanelSpec};
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
    };
    assert!(handler.allow_while_busy("processes"));
    let mut updates = handler.subscribe(app.view.clone()).unwrap();
    let manager = &app.profiles.resources.processes;
    let owner = serde_json::to_string(&(app.view.room.as_str(), app.view.agent.as_str())).unwrap();
    let cancel = CancellationToken::new();
    let process = manager
        .start(
            &owner,
            ProcessStartInput::new("printf 'stdout marker'; printf 'stderr marker' >&2; cat"),
            &cancel,
        )
        .await
        .unwrap()
        .data;
    let hidden = manager
        .start(
            "[\"unbound-room\",\"other-agent\"]",
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
    let list = panel(&app, "").await.unwrap();
    assert_eq!(list.actions.len(), 1);
    assert!(list.body.contains(app.view.room.as_str()));
    assert!(!list.body.contains(&hidden.process_id));
    let open = action(&list, &format!("open:{}", process.process_id));
    let detail = panel(&app, &open).await.unwrap();
    assert!(detail.body.contains("Command:") && detail.body.contains("Owner: code"));
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
    assert_eq!(
        badge(&mut updates, "finished").await.label,
        "Processes: 1 finished"
    );
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
    let list = panel(&app, &action(&stopped, ":forget")).await.unwrap();
    assert!(list.body.contains("No retained background processes"));
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
    );
    let owner = serde_json::to_string(&(app.view.room.as_str(), app.view.agent.as_str())).unwrap();
    let p = manager.start(&owner, ProcessStartInput::new("printf FIRST; i=0; while [ $i -lt 5000 ]; do printf .; i=$((i+1)); done; printf LAST"), &CancellationToken::new()).await.unwrap().data;
    badge(&mut updates, "finished").await;
    let list = panel(&app, "").await.unwrap();
    let tail = panel(&app, &action(&list, &format!("open:{}", p.process_id)))
        .await
        .unwrap();
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
            if updates.borrow_and_update().len() == 1 {
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
