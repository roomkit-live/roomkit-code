//! Persist an explicitly requested preference and recall it in another conversation.
use super::*;

#[tokio::test]
async fn an_explicit_preference_reaches_a_fresh_session_through_project_memory() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let model = Mock::start(vec![
        ("memory", json!({"action":"save","id":"fruit","title":"Fruit preference","text":"The user likes strawberries."})),
        ("", json!({})),
        ("memory", json!({"action":"read","id":"fruit"})),
    ])
    .await;
    let mut settings = settings(root.path(), state.path(), &model.base_url);
    settings.mode = crate::permissions::Mode::Yolo;
    let first = Application::open(&settings, String::new()).await.unwrap();
    submit_prompt(
        &first,
        "save-preference",
        "I like strawberries. Please remember that.",
    )
    .await;
    first.close().await.unwrap();
    drop(first);

    let note =
        crate::storage::project_directory(state.path(), &root.path().canonicalize().unwrap())
            .join("memory/fruit.md");
    assert!(
        std::fs::read_to_string(note)
            .unwrap()
            .contains("strawberries")
    );
    settings.session = Some("another".into());
    let second = Application::open(&settings, String::new()).await.unwrap();
    submit_prompt(&second, "recall-preference", "Which fruit do I like?").await;
    let requests = model.requests.lock().unwrap().clone();
    let before_read = requests[2]["messages"].to_string();
    assert!(before_read.contains("fruit — Fruit preference"));
    assert!(
        !before_read.contains("strawberries"),
        "Only the title is recalled automatically"
    );
    let after_read = requests[3]["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(after_read["role"], "tool");
    assert!(
        after_read["content"]
            .as_str()
            .unwrap()
            .contains("The user likes strawberries.")
    );
    second.close().await.unwrap();
}
