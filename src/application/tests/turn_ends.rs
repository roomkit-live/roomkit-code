//! A turn is told what came before it and its history does not say: that the
//! application restarted since the agent's previous turn, and that this turn
//! ended without a final answer.

use super::*;

/// What a turn reads once, the first after a restart.
const RESTARTED: &str = "The application restarted since your previous turn";
/// What a turn reads after one that did not answer.
const UNANSWERED: &str = "Your previous turn in this conversation did not end with a final answer";

/// What request `index` to the model said.
fn asked(mock: &Mock, index: usize) -> String {
    mock.requests.lock().unwrap()[index]["messages"].to_string()
}

/// Runs a turn whose run fails, up to the end of its operation: a view
/// attached before it would find the run's delivery still closing.
async fn failed_turn(app: &Application, operation: &str) {
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    let Event::Snapshot(snapshot) = event(&client).await else {
        panic!("Expected snapshot")
    };
    let submit = Command::Submit {
        agent: snapshot.selected_agent.clone(),
        text: "Inspect the project.".into(),
    };
    let request = Request::new(
        RequestId::new(1),
        operation,
        snapshot.conversation.clone(),
        submit,
    );
    client.try_send(request).unwrap();
    let mut failed = false;
    loop {
        match event(&client).await {
            Event::RunFinished { outcome, .. } => failed |= outcome == RunOutcome::Failed,
            Event::OperationFinished { operation: id, .. } if id.as_str() == operation => break,
            _ => {}
        }
    }
    assert!(failed, "the run failed");
    drop(client);
    task.await.unwrap().unwrap();
}

/// The processes and the archives of a run of the application go with it, and
/// a later run gives their IDs again: a resumed conversation's history named
/// processes that `process_status` no longer listed, with nothing to say why.
#[tokio::test]
async fn the_first_turn_after_a_restart_is_told_once_what_it_can_no_longer_reach() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    // A known window: each turn's instruction replays where it was read.
    settings.context_window = Some(200_000);
    let app = Application::open(&settings, String::new()).await.unwrap();
    turn_operation(&app, "first").await;
    turn_operation(&app, "second").await;
    app.close().await.unwrap();
    drop(app);
    for request in 0..2 {
        let said = asked(&mock, request);
        assert!(!said.contains(RESTARTED), "the same run: {said}");
        assert!(!said.contains(UNANSWERED), "it answered: {said}");
    }

    let app = Application::open(&settings, String::new()).await.unwrap();
    turn_operation(&app, "after-the-restart").await;
    turn_operation(&app, "later").await;
    app.close().await.unwrap();
    let told = asked(&mock, 2);
    assert_eq!(told.matches(RESTARTED).count(), 1, "{told}");
    assert!(
        told.contains("Processes started and output archives stored before the restart"),
        "{told}"
    );
    let later = asked(&mock, 3);
    assert_eq!(
        later.matches(RESTARTED).count(),
        1,
        "not told again: {later}"
    );
}

/// A turn the provider failed left the model its calls and their results,
/// then the user's next message: nothing told it that the user had read no
/// answer, nor what the application had said of the failure.
#[tokio::test]
async fn a_turn_after_one_that_failed_is_told_how_it_ended_and_what_the_user_read() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![("list", json!({"path":"."})), (REFUSED, json!(null))]).await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.context_window = Some(200_000);
    let app = Application::open(&settings, String::new()).await.unwrap();
    failed_turn(&app, "fails-after-a-call").await;
    // The engine reports the error to its observers apart from the run.
    let ends = &app.profiles.resources.turn_ends;
    let room = app.view.room.as_str();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !ends.holds_failure(room, "code") {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    turn_operation(&app, "after-the-failure").await;
    turn_operation(&app, "later").await;
    app.close().await.unwrap();

    let told = asked(&mock, 2);
    let failed = format!("{UNANSWERED}: it failed on an error.");
    assert_eq!(told.matches(&failed).count(), 1, "{told}");
    assert!(told.contains("The application told the user:"), "{told}");
    assert!(
        told.contains("the user read no conclusion from it"),
        "{told}"
    );
    assert!(!told.contains("private server details"), "{told}");
    assert!(!told.contains(RESTARTED), "the same run: {told}");
    // The turn that follows an answered one is told nothing new.
    let later = asked(&mock, 3);
    assert_eq!(later.matches(UNANSWERED).count(), 1, "{later}");
}
