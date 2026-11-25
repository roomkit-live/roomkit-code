//! A connection edit reaches every agent assembled on it, not only the one
//! the view shows.
use super::*;
use roomkit_tui_engine::CommandResult;

#[tokio::test]
async fn a_rotated_key_reaches_an_agent_that_is_not_selected() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let first = Mock::with_answer(vec![], "First agent answer").await;
    let second = Mock::with_answer(vec![], "Second agent answer").await;
    let settings = settings(root.path(), state.path(), &first.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    add_connection(
        &app,
        &[
            ("id", "secondary"),
            ("label", "Second connection"),
            ("url", &second.base_url),
            ("auth", "stored"),
            ("key", "first-key"),
        ],
    )
    .await;
    edit_profile(
        &mut app,
        "agents",
        "add",
        &[
            ("id", "reviewer"),
            ("label", "Reviewer"),
            ("provider", "secondary"),
            ("model", "second-model"),
            ("context", "32768"),
        ],
    )
    .await;
    turn_operation(&app, "before-rotation").await;
    assert_eq!(
        second.headers.lock().unwrap().as_slice(),
        ["Bearer first-key"]
    );
    let CommandResult::Switch(view) = profile_command(
        &app,
        Command::Invoke {
            name: "agents".into(),
            arguments: "select:code".into(),
        },
    )
    .await
    .unwrap() else {
        panic!("Expected switch")
    };
    app.view = *view;
    // The key changes while another agent is the one shown.
    edit_profile(
        &mut app,
        "providers",
        "edit:secondary",
        &[("key", "rotated-key")],
    )
    .await;
    assert_eq!(app.view.agent.as_str(), "code");
    // The reviewer's next turn, which nothing assembles anew on the way.
    app.view.agent = roomkit::model::ChannelId::new("reviewer").unwrap();
    turn_operation(&app, "after-rotation").await;
    assert_eq!(
        second.headers.lock().unwrap().last().unwrap(),
        "Bearer rotated-key"
    );
    app.close().await.unwrap();
}
