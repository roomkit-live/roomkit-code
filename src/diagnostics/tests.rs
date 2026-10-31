use super::*;

#[test]
fn independent_loggers_serialize_records_and_follow_rotation() -> crate::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("diagnostics.jsonl");
    let first = Diagnostics::open(path.clone(), false)?;
    let second = Diagnostics::open(path.clone(), false)?;
    std::thread::scope(|threads| {
        for log in [&first, &second] {
            threads.spawn(move || {
                for seq in 0..200 {
                    log.record("concurrent_record", json!({"seq":seq}), None);
                }
            });
        }
    });
    let records: Vec<Value> = std::fs::read_to_string(&path)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(
        records
            .iter()
            .filter(|r| r["event"] == "concurrent_record")
            .count(),
        400
    );
    assert_ne!(first.instance, second.instance);
    assert!(!first.state.lock().map_err(|_| "poisoned")?.write_failed);
    assert!(!second.state.lock().map_err(|_| "poisoned")?.write_failed);
    private_file(&path)?.set_len(LIMIT)?;
    first.record("rotated_by_first", json!({}), None);
    second.record("second_follows_rotation", json!({}), None);
    let current = std::fs::read_to_string(&path)?;
    assert!(current.contains("rotated_by_first") && current.contains("second_follows_rotation"));
    assert!(std::fs::metadata(&path)?.len() < LIMIT);
    Ok(())
}

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
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 3);
    assert_eq!(log.state.lock().map_err(|_| "poisoned")?.recent.len(), 100);
    Ok(())
}

#[test]
fn write_failures_remain_visible_in_the_diagnostic_panel() -> crate::Result<()> {
    let directory = tempfile::tempdir()?;
    let log = Diagnostics::open(directory.path().join("diagnostics.jsonl"), false)?;
    // Force a rotation failure while retaining the open current file.
    std::fs::create_dir(log.path.with_extension("previous.jsonl"))?;
    private_file(&log.path)?.set_len(LIMIT)?;
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

#[test]
fn a_panic_kind_names_a_known_failure_never_its_text() {
    let kind = super::panics::kind;
    assert_eq!(
        kind("index out of bounds: the len is 3 but the index is 5"),
        "index out of bounds"
    );
    assert_eq!(
        kind("called `Result::unwrap()` on an `Err` value: Os { code: 2 }"),
        "called `Result::unwrap()` on an `Err` value"
    );
    // Older toolchains quote the string in slicing messages.
    assert_eq!(
        kind("byte index 40 is out of bounds of `my password is hunter2`"),
        "byte index"
    );
    assert_eq!(
        kind("begin <= end (10 <= 3) when slicing `secret`"),
        "begin <= end"
    );
    assert_eq!(
        kind("assertion `left == right` failed\n  left: 1"),
        "assertion `left == right` failed"
    );
    // A message that starts with a value is never kept.
    assert_eq!(kind("hunter2 is not a valid key"), "other");
    assert_eq!(kind(""), "other");
}

#[test]
#[allow(clippy::panic, reason = "The test panics on purpose")]
fn panics_are_recorded_and_counted_without_waiting_on_the_log() -> crate::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("diagnostics.jsonl");
    let log = Diagnostics::open(path.clone(), false)?;
    assert_eq!(log.panic_notice(), None);
    super::report_panics(log.clone());
    let failed = std::thread::Builder::new()
        .name("panic-probe".into())
        .spawn(|| std::panic::panic_any("index out of bounds: the len is 3 but the index is 5"))?
        .join();
    // A panic while this thread holds the log's lock must not wait for it.
    let (done, finished) = std::sync::mpsc::channel();
    let holder = log.clone();
    std::thread::Builder::new()
        .name("locked-probe".into())
        .spawn(move || {
            let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let (_state, _recording) = holder.locked();
                std::panic::panic_any("inside the log's lock")
            }));
            let _ = done.send(unwound.is_err());
        })?;
    let locked = finished.recv_timeout(std::time::Duration::from_secs(5));
    if locked.is_err() {
        // The hook is stuck on the log's lock: restoring the hook or failing
        // here would wait on it too.
        eprintln!("A panic under the log's lock waited for it");
        std::process::abort();
    }
    // Restore the default hook; other tests may panic on purpose meanwhile.
    drop(std::panic::take_hook());
    assert!(failed.is_err());
    assert_eq!(locked, Ok(true));
    let records: Vec<Value> = std::fs::read_to_string(&path)?
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .filter(|record: &Value| record["event"] == "panic")
        .collect();
    let probe = records
        .iter()
        .find(|record| record["thread"] == "panic-probe")
        .ok_or("The panic was not recorded")?;
    assert_eq!(probe["kind"], "index out of bounds");
    assert!(
        probe["location"]
            .as_str()
            .is_some_and(|at| at.contains("diagnostics/tests.rs")),
        "{probe}"
    );
    assert!(
        !records
            .iter()
            .any(|record| record["thread"] == "locked-probe")
    );
    let notice = log.panic_notice().ok_or("No notice")?;
    assert!(notice.starts_with("RoomKit Code hit ") && notice.contains("internal error"));
    assert!(notice.ends_with(&path.display().to_string()));
    // The panel still shows the records after a panic under its lock.
    let current = SessionConfig::new(
        roomkit::model::RoomId::new("test")?,
        roomkit::model::ChannelId::new("agent")?,
        "user",
    );
    let CommandResult::Events(events) = log.panel(&current, "") else {
        return Err("Expected the panel".into());
    };
    assert!(!format!("{events:?}").contains("WRITE FAILED"));
    Ok(())
}
