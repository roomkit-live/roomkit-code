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

#[tokio::test]
async fn a_plan_turn_finds_another_sessions_discussion_without_a_saved_note() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let model = Mock::start(vec![
        ("", json!({})),
        (
            "memory",
            json!({"action":"history", "query":"orchard handbook"}),
        ),
    ])
    .await;
    let mut settings = settings(root.path(), state.path(), &model.base_url);
    settings.mode = crate::permissions::Mode::Plan;
    settings.session = Some("docs-review".into());
    let first = Application::open(&settings, String::new()).await.unwrap();
    submit_prompt(
        &first,
        "discuss",
        "We chose a printed orchard handbook for the offline crew.",
    )
    .await;
    first.close().await.unwrap();
    drop(first);

    settings.session = Some("follow-up".into());
    // The note server can be offline: conversations stay in RoomKit.
    settings.memory = crate::project_memory::Config::Mcp {
        server: "offline".into(),
    };
    let second = Application::open(&settings, String::new()).await.unwrap();
    submit_prompt(
        &second,
        "recall",
        "What did we decide about our orchard handbook?",
    )
    .await;
    {
        let requests = model.requests.lock().unwrap();
        assert!(!requests[1]["messages"].to_string().contains("offline crew"));
        let served = requests[1]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["function"]["name"] == "memory")
            .unwrap();
        let actions = served["function"]["parameters"]["$defs"]["Action"]["enum"]
            .as_array()
            .unwrap();
        assert!(actions.contains(&json!("history")));
        let result = requests[2]["messages"].as_array().unwrap().last().unwrap();
        assert_eq!(result["role"], "tool");
        let text = result["content"].as_str().unwrap();
        assert!(
            text.contains("docs-review#") && text.contains("offline crew"),
            "{text}"
        );
    }
    second.close().await.unwrap();
}
