#![allow(clippy::unwrap_used)]
use super::*;
use std::fs;

#[test]
fn managed_install_update_rollback_and_stale_running_process() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    let installation = Installation { bin: bin.clone() };
    let first = temp.path().join("first");
    fs::write(&first, b"first binary").unwrap();
    let second = temp.path().join("second");
    fs::write(&second, b"second binary").unwrap();
    installation
        .activate(&first, "0.1.0", &install::digest(&first).unwrap(), None)
        .unwrap();
    let running = bin.join("roomkit-code").canonicalize().unwrap();
    assert!(Installation::detect(&running).is_some());
    installation
        .activate(
            &second,
            "0.2.0",
            &install::digest(&second).unwrap(),
            Some(&running),
        )
        .unwrap();
    assert_eq!(
        fs::read(bin.join("roomkit-code")).unwrap(),
        b"second binary"
    );
    assert_eq!(
        fs::read(&running).unwrap(),
        b"first binary",
        "running binary was rewritten"
    );
    assert!(
        installation.ensure_current(&running).is_err(),
        "stale process must not reactivate its state"
    );
    let next = bin.join("roomkit-code").canonicalize().unwrap();
    assert_eq!(installation.previous().unwrap(), "0.1.0");
    installation.rollback(&next, "0.1.0").unwrap();
    assert_eq!(fs::read(bin.join("roomkit-code")).unwrap(), b"first binary");
    assert_eq!(installation.previous().unwrap(), "0.2.0");
}

#[test]
fn integrity_conflicts_and_concurrent_installations_leave_current_intact() {
    use fs2::FileExt;
    let temp = tempfile::tempdir().unwrap();
    let installation = Installation {
        bin: temp.path().join("bin"),
    };
    let first = temp.path().join("first");
    fs::write(&first, "first").unwrap();
    let hash = install::digest(&first).unwrap();
    installation.activate(&first, CURRENT, &hash, None).unwrap();
    let original = installation
        .bin
        .join("roomkit-code")
        .canonicalize()
        .unwrap();
    let second = temp.path().join("second");
    fs::write(&second, "second").unwrap();
    assert!(
        installation
            .activate(&second, "0.2.0", &hash, Some(&original))
            .is_err()
    );
    let lock = fs::File::open(installation.root().join("install.lock")).unwrap();
    lock.lock_exclusive().unwrap();
    assert!(
        installation
            .activate(
                &second,
                "0.2.0",
                &install::digest(&second).unwrap(),
                Some(&original)
            )
            .is_err()
    );
    drop(lock);
    assert_eq!(fs::read(&original).unwrap(), b"first");
    assert_eq!(
        installation
            .bin
            .join("roomkit-code")
            .canonicalize()
            .unwrap(),
        original
    );
    assert!(
        installation
            .activate(&second, "../../bad", &hash, None)
            .is_err()
    );
    fs::write(&original, b"tampered").unwrap();
    assert!(installation.ensure_current(&original).is_err());
    assert!(Installation::detect(&original).is_none());
    let other = Installation {
        bin: temp.path().join("external"),
    };
    fs::create_dir(&other.bin).unwrap();
    fs::write(other.bin.join("roomkit-code"), "unmanaged").unwrap();
    assert!(other.activate(&first, CURRENT, &hash, None).is_err());
    assert_eq!(
        fs::read(other.bin.join("roomkit-code")).unwrap(),
        b"unmanaged"
    );
}

#[test]
fn release_versions_are_numeric_and_reject_paths_and_prereleases() {
    assert!(version("0.10.0").unwrap() > version("v0.9.9").unwrap());
    for bad in [
        "../1.0.0",
        "1.0.0-beta",
        "01.2.3",
        "1.2",
        "v1.2.3\n",
        "1.0.0+dev",
    ] {
        assert!(version(bad).is_err(), "{bad}");
    }
}

#[tokio::test]
async fn rollback_requires_a_fresh_confirmation_scoped_to_the_view() {
    use roomkit_tui::protocol::Event;
    use roomkit_tui_engine::{CommandResult, SessionConfig};
    let temp = tempfile::tempdir().unwrap();
    let installation = Installation {
        bin: temp.path().join("bin"),
    };
    let first = temp.path().join("a");
    fs::write(&first, "first").unwrap();
    let second = temp.path().join("b");
    fs::write(&second, "second").unwrap();
    installation
        .activate(&first, "0.0.1", &install::digest(&first).unwrap(), None)
        .unwrap();
    installation
        .activate(&second, CURRENT, &install::digest(&second).unwrap(), None)
        .unwrap();
    let exe = installation
        .bin
        .join("roomkit-code")
        .canonicalize()
        .unwrap();
    let updates = Arc::new(Updates {
        diagnostics: crate::diagnostics::Diagnostics::open(
            temp.path().join("diagnostics.jsonl"),
            false,
        )
        .unwrap(),
        exe: exe.clone(),
        installation: Some(installation.clone()),
        state: Mutex::new(State::default()),
        changes: tokio::sync::watch::channel(()).0,
        task: Mutex::new(None),
        stop: tokio_util::sync::CancellationToken::new(),
        busy: Arc::new(tokio::sync::Mutex::new(())),
    });
    let view = SessionConfig::new(
        roomkit::model::RoomId::new("a").unwrap(),
        roomkit::model::ChannelId::new("code").unwrap(),
        "user",
    );
    let mut changes = updates.changes.subscribe();
    let result = updates.handle(&view, "rollback").unwrap();
    assert!(
        matches!(result, CommandResult::Events(events) if matches!(&events[0], Event::Panel { .. }))
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), changes.changed())
        .await
        .unwrap()
        .unwrap();
    let token = updates.state().pending.as_ref().unwrap().1.clone();
    assert_eq!(
        installation
            .bin
            .join("roomkit-code")
            .canonicalize()
            .unwrap(),
        exe,
        "review must not activate rollback"
    );
    assert!(updates.handle(&view, "confirm wrong").is_err());
    updates.handle(&view, "dismiss").unwrap();
    assert!(updates.handle(&view, &format!("confirm {token}")).is_err());
    updates.handle(&view, "rollback").unwrap();
    changes.changed().await.unwrap();
    let token = updates.state().pending.as_ref().unwrap().1.clone();
    let other = SessionConfig::new(
        roomkit::model::RoomId::new("b").unwrap(),
        view.agent.clone(),
        "user",
    );
    assert!(updates.handle(&other, &format!("confirm {token}")).is_err());
    updates.handle(&view, &format!("confirm {token}")).unwrap();
    changes.changed().await.unwrap();
    assert_eq!(
        fs::read(installation.bin.join("roomkit-code")).unwrap(),
        b"first"
    );
    assert!(updates.label().contains("restart"));
    assert!(updates.handle(&view, &format!("confirm {token}")).is_err());
    updates.close().await;
}
