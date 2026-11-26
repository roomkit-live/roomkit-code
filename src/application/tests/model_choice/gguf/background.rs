//! A paused Hub response leaves the real command router free to process requests.
use super::*;
use tokio::time::{Duration, timeout};

#[tokio::test]
async fn gguf_background_download_shares_work_and_preserves_the_active_model() {
    let gate = Arc::new(Gate::default());
    let bad = Arc::new(AtomicBool::new(true));
    let (url, server) = hub(bad.clone(), Some(gate.clone())).await;
    let cache = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    crate::local_models::TEST_HUB.scope((url, cache.path().into()), async {
        let app = open_local(root.path(), state.path()).await;
        let menu = model(&app, "").await;
        let search = &offered(&menu).1.iter().find(|c| c.label == "Download in background").unwrap().value;
        let repos = submit_profile(&app, "model", search, &[("query", "coder")]).await.unwrap();
        let repo = &offered(&repos).1[0].value;
        let choices = model(&app, repo).await;
        let download = &offered(&choices).1[0].value;
        let mut changes = app.profiles.local_models.downloads.changes.subscribe();
        let result = timeout(Duration::from_secs(3), model(&app, download)).await.unwrap();
        assert!(matches!(result, CommandResult::Events(ref events) if matches!(&events[..], [Event::Notice { text, .. }] if text.contains("in the background"))));
        timeout(Duration::from_secs(3), gate.entered.notified()).await.unwrap();
        assert!(status(&app).contains("50%"));
        assert!(status(&app).contains("█████░░░░░"));

        // Another command in the same conversation, and a duplicate selection,
        // both return while the Hub still withholds its second shard.
        let picker = timeout(Duration::from_secs(3), model(&app, "")).await.unwrap();
        assert!(!offered(&picker).1.iter().any(|c| c.label == FIRST));
        let _result = model(&app, download).await;
        gate.release.notify_one();
        wait_status(&app, &mut changes, "SHA-256").await;
        assert_eq!(gate.requests.load(Ordering::Relaxed), 1);

        bad.store(false, Ordering::Relaxed);
        let _result = model(&app, download).await;
        assert!(!status(&app).contains("SHA-256"));
        timeout(Duration::from_secs(3), gate.entered.notified()).await.unwrap();
        gate.release.notify_one();
        wait_status(&app, &mut changes, "ready to select").await;
        let missing = format!("gguf - download hf://example/coder@{REV}/missing.gguf");
        let _ = model(&app, &missing).await;
        wait_status(&app, &mut changes, "unavailable").await;
        assert!(status(&app).contains("ready to select"));
        let mut view = app.view.clone();
        app.profiles.settle(&app.kit, &mut view).await;
        assert_eq!(view.model.as_ref().unwrap().model, "initial.gguf");
        let picker = model(&app, "").await;
        let installed = &offered(&picker).1.iter().find(|c| c.label == FIRST).unwrap().value;
        assert!(matches!(model(&app, installed).await, CommandResult::Switch(_)));
        app.close().await.unwrap();
    }).await;
    server.abort();
}

#[tokio::test]
async fn gguf_shutdown_joins_a_download_without_waiting_for_the_hub() {
    let gate = Arc::new(Gate::default());
    let (url, server) = hub(Arc::new(AtomicBool::new(false)), Some(gate.clone())).await;
    let cache = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    crate::local_models::TEST_HUB
        .scope((url, cache.path().into()), async {
            let active = Mock::start(vec![]).await;
            let config = settings(root.path(), state.path(), &active.base_url);
            let app = Application::open(&config, String::new()).await.unwrap();
            submit_profile(
                &app,
                "providers",
                "add",
                &[
                    ("id", "local"),
                    ("kind", "llamacpp"),
                    ("auth", "none"),
                    ("url", ""),
                ],
            )
            .await
            .unwrap();
            let action = format!("gguf local download hf://example/coder@{REV}/{FIRST}");
            let _ = invoke(&app, "providers", &action).await;
            timeout(Duration::from_secs(3), gate.entered.notified())
                .await
                .unwrap();
            turn_operation(&app, "chat-while-downloading").await;
            assert_eq!(active.requests.lock().unwrap()[0]["model"], config.model);
            check_downloads_panel(&app, root.path()).await;
            timeout(Duration::from_secs(5), app.close())
                .await
                .unwrap()
                .unwrap();
            let repo = cache.path().join("models--example--coder");
            assert!(
                repo.join("blobs")
                    .join(crate::digest::sha256_hex(b"one"))
                    .is_file()
            );
            assert!(!repo.join("snapshots").join(REV).join(FIRST).exists());
        })
        .await;
    gate.release.notify_one();
    server.abort();
}

async fn open_local(root: &std::path::Path, state: &std::path::Path) -> Application {
    std::fs::write(root.join("initial.gguf"), b"fixture").unwrap();
    let mut config = settings(root, state, "http://localhost");
    config.base_url.clear();
    config.provider = crate::providers::ProviderKind::LlamaCpp;
    config.model = "initial.gguf".into();
    Application::open(&config, String::new()).await.unwrap()
}

fn status(app: &Application) -> String {
    let section = app.profiles.local_models.downloads.sidebar();
    serde_json::to_string(&section).unwrap()
}

async fn wait_status(
    app: &Application,
    changes: &mut tokio::sync::watch::Receiver<()>,
    text: &str,
) {
    timeout(Duration::from_secs(3), async {
        while !status(app).contains(text) {
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

async fn check_downloads_panel(app: &Application, root: &std::path::Path) {
    use roomkit_tui_engine::CommandHandler;
    let router = app.router(root);
    let request = Request::new(
        RequestId::new(2),
        "download-progress",
        app.view.room.as_str(),
        Command::Invoke {
            name: "downloads".into(),
            arguments: String::new(),
        },
    );
    let CommandResult::Events(events) = router.handle(request, app.view.clone()).await.unwrap()
    else {
        panic!("Expected the download panel");
    };
    let [Event::Panel { panel, .. }] = &events[..] else {
        panic!("Expected the download panel")
    };
    assert!(panel.body.contains("50%") && panel.body.contains("3 B / 6 B"));
    let dismiss = Request::new(
        RequestId::new(3),
        "close-downloads",
        app.view.room.as_str(),
        Command::Invoke {
            name: panel.command.clone(),
            arguments: panel.dismiss.clone(),
        },
    );
    assert!(
        matches!(router.handle(dismiss, app.view.clone()).await.unwrap(), CommandResult::Events(events) if events.is_empty())
    );
}
