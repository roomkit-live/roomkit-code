#![allow(clippy::unwrap_used, reason = "Invalid fixtures fail the test")]

use super::*;
use clap::Parser;
use roomkit_tui::{Session, protocol::*};

fn setup() -> (tempfile::TempDir, Resume, Session) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project with spaces");
    std::fs::create_dir(&project).unwrap();
    let config = root.path().join("settings.toml");
    std::fs::write(&config, "[ui]\nname = 'Example'\n").unwrap();
    let cli = crate::config::Cli::try_parse_from([
        "roomkit-code",
        project.to_str().unwrap(),
        "--state-dir",
        root.path().join("storage").to_str().unwrap(),
        "--config",
        config.to_str().unwrap(),
        "--mode",
        "ask",
        "--color",
        "never",
        "--theme",
        "terminal",
        "--no-terminal-status",
        "--no-animations",
        "--native-selection",
        "--no-logo",
        "--debug",
    ])
    .unwrap();
    let settings = Settings::load(&cli).unwrap();
    let mut session = Session::default();
    session.apply(Event::Snapshot({
        let mut snapshot = Snapshot::new("first", "Project title", Actor::new("coder", "Coder"));
        snapshot.can_interrupt = true;
        snapshot.commands = vec![CommandSpec::new("mode", "Policy")];
        snapshot.mode =
            Some(crate::permissions::Policy::new(crate::permissions::Mode::Ask).state("first"));
        snapshot
    }));
    (root, Resume::new(&settings), session)
}

#[test]
fn resume_follows_the_displayed_session_and_confirmed_mode_with_explicit_options() {
    let (root, resume, mut session) = setup();
    let initial = resume.report(session.observation().unwrap());
    let args = initial.session.resume.unwrap();
    assert_eq!(
        &args[..8],
        [
            "roomkit-code",
            root.path().join("project with spaces").to_str().unwrap(),
            "--state-dir",
            root.path().join("storage").to_str().unwrap(),
            "--session",
            "first",
            "--mode",
            "ask"
        ]
    );
    assert!(args.windows(2).any(|pair| pair
        == [
            "--config",
            root.path().join("settings.toml").to_str().unwrap()
        ]));
    for flag in [
        "--no-terminal-status",
        "--no-animations",
        "--native-selection",
        "--no-logo",
        "--debug",
    ] {
        assert!(args.iter().any(|arg| arg == flag));
    }
    session.apply(Event::OperationStarted {
        conversation: "first".into(),
        operation: "op".into(),
    });
    assert_eq!(
        resume.report(session.observation().unwrap()).state,
        State::Working
    );
    let mut mode = session.snapshot().unwrap().mode.clone().unwrap();
    mode.selected = "yolo".into();
    session.apply(Event::ModeChanged {
        conversation: "foreign".into(),
        mode: mode.clone(),
    });
    assert!(
        resume
            .report(session.observation().unwrap())
            .session
            .resume
            .unwrap()
            .iter()
            .any(|arg| arg == "ask")
    );
    session.apply(Event::ModeChanged {
        conversation: "first".into(),
        mode,
    });
    let changed = resume.report(session.observation().unwrap());
    assert_eq!(changed.state, State::Working);
    assert!(
        changed
            .session
            .resume
            .unwrap()
            .windows(2)
            .any(|pair| pair == ["--mode", "yolo"])
    );
    session.apply(Event::RunStarted {
        conversation: "first".into(),
        run: "run".into(),
    });
    session.apply(Event::PermissionRequested {
        conversation: "first".into(),
        permission: Permission::new("approval", "coder", "run", "Change?", "Private"),
    });
    let blocked = resume.report(session.observation().unwrap());
    assert_eq!(blocked.state, State::Blocked);
    assert!(!format!("{blocked:?}").contains("Private"));
    let mut restored = session.snapshot().cloned().unwrap();
    restored.conversation = "second".into();
    restored.active_run = None;
    restored.permissions.clear();
    session.apply(Event::Snapshot(restored));
    let switched = resume.report(session.observation().unwrap());
    assert_eq!(switched.session.id, "second");
    assert_eq!(switched.state, State::Idle);
    assert!(
        switched
            .session
            .resume
            .unwrap()
            .windows(2)
            .any(|pair| pair == ["--session", "second"])
    );
}

#[test]
fn unrepresentable_paths_disable_only_resume() {
    let (_root, mut resume, session) = setup();
    resume.prefix.as_mut().unwrap()[1] = "/tmp/owner's project".into();
    let report = resume.report(session.observation().unwrap());
    assert!(report.session.resume.is_none());
    assert_eq!(report.state, State::Idle);
    resume.prefix = None;
    assert!(
        resume
            .report(session.observation().unwrap())
            .session
            .resume
            .is_none()
    );
}
