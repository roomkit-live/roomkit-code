//! Drive cancellation, explicit selection and deletion through the offered panel actions.
use super::*;
use roomkit_tui::protocol::PanelSpec;
use tokio::time::{Duration, timeout};

fn panel(result: &CommandResult) -> &PanelSpec {
    let CommandResult::Events(events) = result else {
        panic!("expected a panel")
    };
    let [Event::Panel { panel, .. }] = events.as_slice() else {
        panic!("expected a panel event")
    };
    panel
}

fn action(result: &CommandResult, label: &str) -> String {
    panel(result)
        .actions
        .iter()
        .find(|a| a.label.starts_with(label))
        .unwrap()
        .value
        .clone()
}

async fn downloads(app: &Application, args: &str) -> std::result::Result<CommandResult, String> {
    app.profiles
        .manage_downloads(&app.kit, app.view.clone(), args)
        .await
}

#[tokio::test]
async fn gguf_cancel_releases_the_transfer_and_a_stale_cancel_cannot_stop_the_retry() {
    let gate = Arc::new(Gate::default());
    let (url, server) = hub(Arc::new(AtomicBool::new(false)), Some(gate.clone())).await;
    let cache = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    crate::local_models::TEST_HUB.scope((url, cache.path().into()), async {
        let app = open_local(root.path(), state.path()).await;
        let mut changes = app.profiles.local_models.downloads.changes.subscribe();
        let select = format!("gguf - use hf://example/coder@{REV}/{FIRST}");
        // A prior interrupted attempt left a prefix for the blocked second shard.
        let blobs = cache.path().join("models--example--coder/blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        let second_prefix = blobs.join(crate::digest::sha256_hex(b"two")).with_extension("incomplete");
        std::fs::write(&second_prefix, b"t").unwrap();
        let first_blob = blobs.join(crate::digest::sha256_hex(b"one"));
        let started = model(&app, &select).await;
        let cancel = action(&started, "Cancel ");
        timeout(Duration::from_secs(3), gate.entered.notified()).await.unwrap();
        let mut events = vec![Event::BackgroundChanged { conversation: app.view.room.as_str().into(), status: None }];
        app.profiles.local_models.downloads.show_progress(&mut events);
        assert!(matches!(&events[0], Event::BackgroundChanged { status: Some(s), .. } if s.label.contains("█████░░░░░ 50%") && s.command == "downloads"));
        assert!(first_blob.is_file() && second_prefix.is_file());
        downloads(&app, &cancel).await.unwrap();
        background::wait_status(&app, &mut changes, "cancelled").await;
        assert!(!first_blob.exists() && !second_prefix.exists());
        assert!(offered(&downloads(&app, "installed").await.unwrap()).1.is_empty());
        let retry = model(&app, &select).await;
        assert_ne!(action(&retry, "Cancel "), cancel);
        assert!(downloads(&app, &cancel).await.is_err());
        timeout(Duration::from_secs(3), gate.entered.notified()).await.unwrap();
        gate.release.notify_waiters();
        background::wait_status(&app, &mut changes, "ready to select").await;
        let use_file = action(&downloads(&app, "").await.unwrap(), "Use ");
        assert!(matches!(downloads(&app, &use_file).await.unwrap(), CommandResult::Switch(_)));
        let token = confirm_removal(&app).await;
        assert!(downloads(&app, &token).await.unwrap_err().contains("selected"));
        app.close().await.unwrap();
    }).await;
    server.abort();
}

async fn confirm_removal(app: &Application) -> String {
    let listing = downloads(app, "installed").await.unwrap();
    let details = downloads(app, &offered(&listing).1[0].value).await.unwrap();
    let confirmation = downloads(app, &action(&details, "Delete from disk"))
        .await
        .unwrap();
    assert_eq!(panel(&confirmation).actions[0].label, "Keep model");
    action(&confirmation, "Delete from disk")
}

#[tokio::test]
async fn gguf_deletion_needs_confirmation_and_only_removes_unshared_weights() {
    let (url, server) = hub(Arc::new(AtomicBool::new(false)), None).await;
    let cache = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    crate::local_models::TEST_HUB
        .scope((url, cache.path().into()), async {
            let app = open_local(root.path(), state.path()).await;
            let mut changes = app.profiles.local_models.downloads.changes.subscribe();
            let select = format!("gguf - download hf://example/coder@{REV}/{FIRST}");
            let _ = model(&app, &select).await;
            background::wait_status(&app, &mut changes, "ready to select").await;
            let repo = cache.path().join("models--example--coder");
            let second = repo.join("snapshots").join("a".repeat(40));
            std::fs::create_dir_all(&second).unwrap();
            let shared = repo.join("blobs").join(crate::digest::sha256_hex(b"one"));
            let other = second.join("other.gguf");
            std::os::unix::fs::symlink(&shared, &other).unwrap();
            let token = confirm_removal(&app).await;
            downloads(&app, "close").await.unwrap();
            assert!(downloads(&app, &token).await.is_err());
            assert_eq!(
                std::fs::read(repo.join("snapshots").join(REV).join(LAST)).unwrap(),
                b"two"
            );
            let token = confirm_removal(&app).await;
            let left = downloads(&app, &token).await.unwrap();
            assert_eq!(offered(&left).1.len(), 1);
            assert_eq!(offered(&left).1[0].label, "other.gguf");
            assert!(
                !repo
                    .join("blobs")
                    .join(crate::digest::sha256_hex(b"two"))
                    .exists()
            );
            assert_eq!(std::fs::read(other).unwrap(), b"one");
            assert!(downloads(&app, &token).await.is_err());
            app.close().await.unwrap();
        })
        .await;
    server.abort();
}
