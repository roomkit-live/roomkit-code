//! A room's numeric effort setting survives a reopen and leaves the agent default intact.
use super::*;

#[tokio::test]
async fn conversation_budget_can_be_set_before_loading_and_cleared_after_reopen() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = open_local(root.path(), state.path()).await;
    edit_profile(
        &mut app,
        "agents",
        "edit:code",
        &[("output", "4096"), ("thinking_budget", "512")],
    )
    .await;
    let menu = invoke(&app, "effort", "").await;
    assert_eq!(values(offered(&menu).1), ["budget", "detect"]);
    assert!(offered(&menu).1[0].label.contains("512 tokens"));
    let view = app.view.room.clone();
    edit_profile(&mut app, "effort", "budget", &[("budget", "128")]).await;
    assert_eq!(
        app.view.model.as_ref().unwrap().effort.as_deref(),
        Some("128 tokens")
    );
    assert_eq!(
        opened(&invoke(&app, "agents", "edit:code").await)["thinking_budget"],
        "512"
    );
    let error = submit_profile(&app, "effort", "budget", &[("budget", "4096")])
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("less than the output budget"));
    assert_eq!(
        opened(&invoke(&app, "effort", "budget").await)["budget"],
        "128"
    );
    app.close().await.unwrap();
    drop(app);
    let mut settings = settings(root.path(), state.path(), "http://localhost");
    settings.session = Some("other".into());
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_ne!(app.view.room, view);
    assert_eq!(
        opened(&invoke(&app, "effort", "budget").await)["budget"],
        ""
    );
    assert_eq!(
        app.view.model.as_ref().unwrap().effort.as_deref(),
        Some("512 tokens")
    );
    app.close().await.unwrap();
    drop(app);
    settings.session = Some(view.to_string());
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(
        opened(&invoke(&app, "effort", "budget").await)["budget"],
        "128"
    );
    edit_profile(&mut app, "effort", "budget", &[("budget", "")]).await;
    assert_eq!(
        app.view.model.as_ref().unwrap().effort.as_deref(),
        Some("512 tokens")
    );
    let CommandResult::Switch(next) = invoke(&app, "effort", "0").await else {
        panic!("numeric effort")
    };
    assert_eq!(
        next.model.as_ref().unwrap().effort.as_deref(),
        Some("0 tokens")
    );
    app.close().await.unwrap();
}
