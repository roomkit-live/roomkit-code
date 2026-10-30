use super::*;

#[test]
fn logs_rotate_privately_and_never_copy_raw_errors() -> crate::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("diagnostics.jsonl");
    let log = Diagnostics::open(path.clone(), false)?;
    log.record("engine_error", json!({"stage":"streaming"}), Some(
        "the provider's answer is malformed: Responses terminal text differs from its streamed text; private prompt and secret bearer"
    ));
    let first = std::fs::read_to_string(&path)?;
    assert!(first.contains("responses_text_mismatch"));
    assert!(!first.contains("private prompt") && !first.contains("secret bearer"));
    for _ in 0..10_000 {
        log.record("engine_error", json!({}), Some("HTTP 401 private-secret"));
    }
    let previous = path.with_extension("previous.jsonl");
    for file in [&path, &previous] {
        assert!(std::fs::metadata(file)?.len() <= LIMIT);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(file)?.permissions().mode() & 0o777, 0o600);
        }
        let lines = std::fs::read_to_string(file)?;
        assert!(!lines.contains("private-secret"));
        for line in lines.lines() {
            serde_json::from_str::<Value>(line)?;
        }
    }
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 2);
    assert_eq!(log.state.lock().map_err(|_| "poisoned")?.recent.len(), 100);
    Ok(())
}

#[test]
fn write_failures_remain_visible_in_the_diagnostic_panel() -> crate::Result<()> {
    let directory = tempfile::tempdir()?;
    let log = Diagnostics::open(directory.path().join("diagnostics.jsonl"), false)?;
    // Force a rotation failure while retaining the open current file.
    std::fs::create_dir(log.path.with_extension("previous.jsonl"))?;
    log.state.lock().map_err(|_| "poisoned")?.bytes = LIMIT;
    log.record("engine_error", json!({}), Some("HTTP 429 secret"));
    let current = SessionConfig::new(
        roomkit::model::RoomId::new("test")?,
        roomkit::model::ChannelId::new("agent")?,
        "user",
    );
    let CommandResult::Events(events) = log.panel(&current, "") else {
        return Err("Expected panel".into());
    };
    let body = serde_json::to_string(&events)?;
    assert!(body.contains("WRITE FAILED") && body.contains("provider_quota"));
    assert!(!body.contains("secret"));
    Ok(())
}
