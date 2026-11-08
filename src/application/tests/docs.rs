//! The installed guide reaches the model through the documentation tools.
use super::*;
use crate::permissions::Mode;

fn result(session: &Session, name: &str) -> Value {
    let tool = session
        .snapshot()
        .unwrap()
        .items
        .iter()
        .find_map(|item| match item {
            Item::Tool { call, .. } if call.name == name => Some(call),
            _ => None,
        })
        .unwrap();
    assert_eq!(tool.state, ToolState::Succeeded, "{}: {}", name, tool.text);
    tool.data.as_ref().unwrap()["data"].clone()
}

#[tokio::test]
async fn agents_search_and_read_the_installed_guide_without_approval_even_in_plan_mode() {
    for mode in [Mode::Ask, Mode::Plan] {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mock = Mock::start(vec![
            ("docs_search", json!({"query":"command hooks"})),
            (
                "docs_read",
                json!({"document_id":"command-hooks.md","limit":3}),
            ),
        ])
        .await;
        let mut settings = settings(root.path(), state.path(), &mock.base_url);
        settings.mode = mode;
        let app = Application::open(&settings, String::new()).await.unwrap();
        // A pending approval or a host refusal would fail the turn or this lookup.
        let session = turn(&app).await;
        let found = result(&session, "docs_search");
        assert_eq!(
            found["matches"][0]["document"]["document_id"],
            "command-hooks.md"
        );
        let page = result(&session, "docs_read");
        assert_eq!(page["lines"][0]["text"], "# Command hooks");
        assert_eq!(page["document"]["version"], env!("CARGO_PKG_VERSION"));
        let published: Vec<_> = std::fs::read_dir(state.path().join("docs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(published.len(), 1);
        assert!(published[0].join("command-hooks.md").is_file());
        let prompt = mock.requests.lock().unwrap()[0]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(prompt.contains("RoomKit Code's own user guide"));
        app.close().await.unwrap();
    }
}
