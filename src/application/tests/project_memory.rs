//! Persist a fact and recall it through a fresh conversation and profile assembly.
use super::*;

#[tokio::test]
async fn memory_markdown_reaches_a_fresh_session_without_replaying_the_old_turn() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let model = Mock::start(vec![(
        "memory",
        json!({"action":"save","id":"ci","title":"Windows CI","text":"Use the blue runner"}),
    )])
    .await;
    let mut settings = settings(root.path(), state.path(), &model.base_url);
    settings.mode = crate::permissions::Mode::Yolo;
    let first = Application::open(&settings, String::new()).await.unwrap();
    turn(&first).await;
    first.close().await.unwrap();
    drop(first);
    let path =
        crate::storage::project_directory(state.path(), &root.path().canonicalize().unwrap())
            .join("memory/ci.md");
    assert!(
        std::fs::read_to_string(path)
            .unwrap()
            .contains("Use the blue runner")
    );
    settings.session = Some("another".into());
    let second = Application::open(&settings, String::new()).await.unwrap();
    turn(&second).await;
    let wire = model.requests.lock().unwrap().last().unwrap()["messages"].to_string();
    assert!(wire.contains("ci — Windows CI"), "{wire}");
    assert!(
        !wire.contains("Use the blue runner"),
        "Only titles are recalled automatically"
    );
    second.close().await.unwrap();
}
