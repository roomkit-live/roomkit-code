//! A start that stops part-way: what it opened is released and recorded,
//! and a session it left half created is completed by the next launch.
use super::*;

/// A room created but neither bound nor designating an agent, as a launch
/// killed between the two leaves it, was accepted as it was: the next
/// launch then refused it as not accessible to this terminal.
#[tokio::test]
async fn a_session_created_part_way_is_completed_by_the_next_launch() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let mut settings = settings(project.path(), state.path(), &mock.base_url);
    settings.session = None;
    let app = Application::open(&settings, String::new()).await.unwrap();
    let room = roomkit::model::RoomId::new("left-part-way").unwrap();
    app.kit
        .create_room(roomkit::CreateRoomOptions {
            room_id: Some(room.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    app.close().await.unwrap();
    drop(app);
    settings.session = Some("left-part-way".into());
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(app.view.room, room);
    let designated = crate::sessions::designated(&app.kit, &room).await.unwrap();
    assert_eq!(designated.as_ref(), Some(&app.view.agent));
    turn(&app).await;
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
    app.close().await.unwrap();
}

/// A start that failed after the profiles were loaded closed nothing and
/// left no trace of where it stopped.
#[tokio::test]
async fn a_start_that_fails_records_where_it_stopped() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(project.path(), state.path(), &mock.base_url);
    let database = {
        let storage = Storage::open(&settings.state_dir, &settings.workspace).unwrap();
        storage.database.clone()
    };
    std::fs::write(&database, b"not a database").unwrap();
    assert!(Application::open(&settings, String::new()).await.is_err());
    let log = std::fs::read_to_string(database.with_file_name("diagnostics.jsonl")).unwrap();
    let failed: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("\"application_start_failed\""))
        .collect();
    assert_eq!(failed.len(), 1, "{log}");
    assert!(failed[0].contains("\"stage\":\"engine\""), "{log}");
    assert!(!failed[0].contains("not a database"), "{log}");
    // What the failed start held is released: the next one opens.
    std::fs::remove_file(&database).unwrap();
    let app = Application::open(&settings, String::new()).await.unwrap();
    app.close().await.unwrap();
}

/// A launch in a session group whose Redis did not answer did not start:
/// the project's own work needs no broker.
#[tokio::test]
async fn a_launch_opens_without_the_session_group_it_cannot_reach() {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let mut settings = settings(project.path(), state.path(), &mock.base_url);
    settings.session = Some("alone".into());
    settings.session_group = Some("team".into());
    // Nothing listens on the discard port.
    settings.executor_url = Some("redis://127.0.0.1:9".into());
    let app = Application::open(&settings, String::new()).await.unwrap();
    // The conversation says so as it opens, not only its session tools.
    let opened = app.session().snapshot().await.unwrap();
    let told = opened.items.iter().any(|item| {
        matches!(item, Item::Message(m) if m.text.starts_with("Session group team could not be reached"))
    });
    assert!(told, "{:?}", opened.items);
    turn(&app).await;
    let log =
        std::fs::read_to_string(app._storage.database.with_file_name("diagnostics.jsonl")).unwrap();
    assert!(log.contains("\"session_group_unavailable\""), "{log}");
    assert!(!log.contains("127.0.0.1:9"), "{log}");
    app.close().await.unwrap();
}
