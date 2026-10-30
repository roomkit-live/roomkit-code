#![allow(clippy::unwrap_used, reason = "Invalid fixtures fail the test")]

use super::*;

fn endpoint(root: &std::path::Path) -> Endpoint {
    Endpoint {
        binary: root.join("fake-herdr"),
        socket: root.join("socket").into_os_string(),
        pane: "w1:p1".into(),
        storage: root.join("global-integration-state"),
    }
}

fn report(state: State) -> Report {
    Report {
        state,
        title: "Fixture title".into(),
        session: SessionRef {
            id: "first".into(),
            resume: Some(
                [
                    "roomkit-code",
                    "/tmp/project with spaces",
                    "--session",
                    "first",
                    "--mode",
                    "ask",
                ]
                .map(String::from)
                .to_vec(),
            ),
        },
    }
}

#[test]
fn activation_requires_every_herdr_value_and_user_storage() {
    let values = [
        ("HERDR_ENV", "1"),
        ("HERDR_BIN_PATH", "/tmp/herdr"),
        ("HERDR_SOCKET_PATH", "/tmp/herdr.sock"),
        ("HERDR_PANE_ID", "w1:p1"),
    ];
    for missing in [
        None,
        Some("HERDR_ENV"),
        Some("HERDR_BIN_PATH"),
        Some("HERDR_SOCKET_PATH"),
        Some("HERDR_PANE_ID"),
    ] {
        let found = Endpoint::from_environment(Some("/tmp/user".into()), |key| {
            values
                .iter()
                .find(|(name, _)| *name == key && missing != Some(key))
                .map(|(_, value)| OsString::from(value))
        });
        assert_eq!(found.is_some(), missing.is_none());
    }
    assert!(Endpoint::from_environment(None, |_| Some("1".into())).is_none());
    assert!(Endpoint::from_environment(Some("/tmp/user".into()), |_| Some("0".into())).is_none());
}

#[test]
fn sequences_survive_restart_clock_rollback_and_conflicting_instances() {
    let root = tempfile::tempdir().unwrap();
    let endpoint = endpoint(root.path());
    let mut first = sequence::Sequence::open(&endpoint).unwrap();
    assert_eq!(first.reserve_for_test(9000).unwrap(), 9000);
    assert!(sequence::Sequence::open(&endpoint).is_err());
    drop(first);
    let mut restarted = sequence::Sequence::open(&endpoint).unwrap();
    assert_eq!(restarted.reserve_for_test(10).unwrap(), 9001);
    drop(restarted);
    let path = std::fs::read_dir(&endpoint.storage)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    std::fs::write(path, "corrupt").unwrap();
    assert!(
        sequence::Sequence::open(&endpoint).is_err(),
        "Never reset a damaged high-water mark"
    );
}

#[test]
fn invalid_resume_commands_are_rejected_without_changing_state() {
    for args in [
        vec![],
        vec!["/tmp/code".into()],
        vec!["code".into(), "owner's project".into()],
        vec!["code".into(), "line\nnext".into()],
        vec!["code".into(); 65],
        vec!["code".into(), "x".repeat(8192)],
    ] {
        assert!(!valid_resume(&args));
    }
    assert!(valid_resume(&[
        "roomkit-code".into(),
        "/tmp/project with spaces".into()
    ]));
    let mut sent = Sent::default();
    let valid = report(State::Working);
    sent.applied(Action::State, &valid);
    let mut invalid = valid.clone();
    invalid.session.resume = None;
    assert!(matches!(sent.next(&invalid), Some(Action::ClearResume)));
    sent.applied(Action::ClearResume, &invalid);
    assert!(matches!(sent.next(&invalid), Some(Action::State)));
    assert!(!Action::State.args(&invalid).iter().any(|arg| arg == "--"));
}

#[cfg(unix)]
mod worker {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Fixture {
        root: tempfile::TempDir,
        desired: Arc<Mutex<Report>>,
        errors: Arc<Mutex<Vec<&'static str>>>,
    }
    impl Fixture {
        fn new(mode: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("mode"), mode).unwrap();
            let program = format!(
                "#!/usr/bin/env python3\nimport json, pathlib, sys, time\nroot = pathlib.Path({})\nwith (root / 'calls').open('a') as log:\n log.write(json.dumps(sys.argv[1:]) + '\\n')\nmode = (root / 'mode').read_text()\nif mode == 'stall': time.sleep(10)\nif mode == 'fail':\n print('PRIVATE RAW FAILURE', file=sys.stderr)\n sys.exit(1)\n",
                serde_json::to_string(root.path().to_str().unwrap()).unwrap()
            );
            let path = root.path().join("fake-herdr");
            std::fs::write(&path, program).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                root,
                desired: Arc::new(Mutex::new(report(State::Idle))),
                errors: Arc::default(),
            }
        }
        fn start(&self) -> Reporter {
            let desired = self.desired.clone();
            let errors = self.errors.clone();
            Reporter::start(
                endpoint(self.root.path()),
                move || Some(desired.lock().unwrap().clone()),
                move |code| errors.lock().unwrap().push(code),
            )
        }
        fn calls(&self) -> Vec<Vec<String>> {
            std::fs::read_to_string(self.root.path().join("calls"))
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        }
        async fn wait(&self, predicate: impl Fn(&[Vec<String>]) -> bool) {
            tokio::time::timeout(Duration::from_secs(6), async {
                while !predicate(&self.calls()) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn reports_are_deduplicated_and_release_follows_frozen_application_cleanup() {
        let fixture = Fixture::new("ok");
        let reporter = fixture.start();
        fixture.wait(|calls| calls.len() >= 2).await;
        let first = fixture.calls();
        assert_eq!(first[0][1], "report-agent");
        assert_eq!(first[1][1], "report-metadata");
        assert!(first[0].windows(2).any(|pair| pair == ["--state", "idle"]));
        assert!(
            first[1]
                .windows(2)
                .any(|pair| pair == ["--applies-to-source", SOURCE])
        );
        fixture.desired.lock().unwrap().state = State::Working;
        fixture.wait(|calls| calls.len() >= 3).await;
        {
            let mut desired = fixture.desired.lock().unwrap();
            *desired.session.resume.as_mut().unwrap().last_mut().unwrap() = "yolo".into();
            desired.title = "New title".into();
        }
        fixture.wait(|calls| calls.len() >= 5).await;
        let stable = fixture.calls();
        assert_eq!(stable[3][1], "report-agent-session");
        assert_eq!(stable[4][1], "report-metadata");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(fixture.calls(), stable);
        reporter.freeze();
        fixture.desired.lock().unwrap().state = State::Idle;
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(fixture.calls(), stable);
        reporter.close().await;
        let calls = fixture.calls();
        assert!(
            calls[calls.len() - 2]
                .iter()
                .any(|arg| arg == "--clear-title")
        );
        assert_eq!(calls.last().unwrap()[1], "release-agent");
        let sequences: Vec<u64> = calls
            .iter()
            .map(|args| {
                args.windows(2).find(|pair| pair[0] == "--seq").unwrap()[1]
                    .parse()
                    .unwrap()
            })
            .collect();
        assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(fixture.calls(), calls);
    }

    #[tokio::test]
    async fn failures_retry_only_the_latest_desired_state_and_timeouts_do_not_stall_shutdown() {
        for (mode, code) in [("fail", "report_failed"), ("stall", "report_timeout")] {
            let fixture = Fixture::new(mode);
            let reporter = fixture.start();
            fixture.wait(|calls| !calls.is_empty()).await;
            for state in [State::Working, State::Idle, State::Blocked] {
                fixture.desired.lock().unwrap().state = state;
            }
            tokio::time::timeout(Duration::from_secs(3), async {
                while !fixture.errors.lock().unwrap().contains(&code) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            std::fs::write(fixture.root.path().join("mode"), "ok").unwrap();
            fixture.wait(|calls| calls.len() >= 3).await;
            let calls = fixture.calls();
            assert!(
                calls[1]
                    .windows(2)
                    .any(|pair| pair == ["--state", "blocked"])
            );
            assert_eq!(calls[2][1], "report-metadata");
            std::fs::write(fixture.root.path().join("mode"), "stall").unwrap();
            tokio::time::timeout(Duration::from_secs(3), reporter.close())
                .await
                .unwrap();
            assert_eq!(fixture.calls().last().unwrap()[1], "release-agent");
        }
    }
}
