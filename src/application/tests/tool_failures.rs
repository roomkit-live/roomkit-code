//! A tool call that fails is the tool's outcome, not the engine's.
use super::*;

/// A failed `read` is recorded with the tool's outcome: the engine's
/// classifier, which knows provider and engine failures, does not read it.
#[tokio::test]
async fn a_failed_tool_call_is_recorded_as_the_tools_not_as_an_engine_failure() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    let mock = Mock::start(vec![("read", json!({"path": "src"}))]).await;
    let app = Application::open(
        &settings(root.path(), state.path(), &mock.base_url),
        String::new(),
    )
    .await
    .unwrap();
    turn(&app).await;
    app.close().await.unwrap();
    let log =
        std::fs::read_to_string(app._storage.database.with_file_name("diagnostics.jsonl")).unwrap();
    let finished: Vec<Value> = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["event"] == "tool_finished")
        .collect();
    assert_eq!(finished.len(), 1, "{log}");
    assert_eq!(finished[0]["tool"], "read");
    assert_eq!(finished[0]["outcome"], "failed");
    assert!(finished[0].get("code").is_none(), "{log}");
    assert!(!log.contains("engine_failed"), "{log}");
}
