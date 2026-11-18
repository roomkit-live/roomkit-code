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
    let initial = resume.report(session.observation().unwrap(), Background::default());
    let args = initial.session.resume.unwrap();
    assert_eq!(
        &args[..8],
        [
            "roomkit-code",
            root.path()
                .join("project with spaces")
                .canonicalize()
                .unwrap()
                .to_str()
                .unwrap(),
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
        resume
            .report(session.observation().unwrap(), Background::default())
            .state,
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
            .report(session.observation().unwrap(), Background::default())
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
    let changed = resume.report(session.observation().unwrap(), Background::default());
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
        answers: None,
    });
    session.apply(Event::PermissionRequested {
        conversation: "first".into(),
        permission: Permission::new("approval", "coder", "run", "Change?", "Private"),
    });
    let blocked = resume.report(session.observation().unwrap(), Background::default());
    assert_eq!(blocked.state, State::Blocked);
    assert!(!format!("{blocked:?}").contains("Private"));
    let mut restored = session.snapshot().cloned().unwrap();
    restored.conversation = "second".into();
    restored.active_run = None;
    restored.permissions.clear();
    session.apply(Event::Snapshot(restored));
    let switched = resume.report(session.observation().unwrap(), Background::default());
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
    let report = resume.report(session.observation().unwrap(), Background::default());
    assert!(report.session.resume.is_none());
    assert_eq!(report.state, State::Idle);
    resume.prefix = None;
    assert!(
        resume
            .report(session.observation().unwrap(), Background::default())
            .session
            .resume
            .is_none()
    );
}

#[test]
fn a_person_reads_the_command_without_the_default_storage() {
    let root = tempfile::tempdir().unwrap();
    let cli = crate::config::Cli::try_parse_from([
        "roomkit-code",
        root.path().to_str().unwrap(),
        "--state-dir",
        root.path().join("storage").to_str().unwrap(),
    ])
    .unwrap();
    let mut settings = Settings::load(&cli).unwrap();
    let named = Resume::shown(&settings).command("room", "ask").unwrap();
    assert!(named.iter().any(|arg| arg == "--state-dir"), "{named:?}");
    settings.default_storage = true;
    let default = Resume::shown(&settings).command("room", "ask").unwrap();
    let workspace = root.path().canonicalize().unwrap();
    assert_eq!(
        default,
        [
            "roomkit-code",
            workspace.to_str().unwrap(),
            "--session",
            "room",
            "--mode",
            "ask"
        ]
    );
    // Herdr's command runs later, possibly from another environment.
    let full = Resume::new(&settings).command("room", "ask").unwrap();
    assert!(full.iter().any(|arg| arg == "--state-dir"), "{full:?}");
}

#[test]
fn a_question_waiting_for_the_person_blocks_like_an_approval() {
    let (_root, resume, mut session) = setup();
    session.apply(Event::RunStarted {
        conversation: "first".into(),
        run: "run".into(),
        answers: None,
    });
    let items = vec![QuestionItem::new(
        "Database",
        "Which database?",
        vec![QuestionOption::new("SQLite", "")],
    )];
    session.apply(Event::QuestionAsked {
        conversation: "first".into(),
        question: Question::new("question", "coder", "run", items),
    });
    assert_eq!(
        resume
            .report(session.observation().unwrap(), Background::default())
            .state,
        State::Blocked
    );
    session.apply(Event::QuestionResolved {
        conversation: "first".into(),
        question: "question".into(),
    });
    assert_eq!(
        resume
            .report(session.observation().unwrap(), Background::default())
            .state,
        State::Working
    );
}

/// A delegated task's result starts a turn of its own: while one runs the
/// application is working for Herdr, after its delegating agent's turn
/// ended. Processes keep no agent working, a server running until closing:
/// they label the idle state. An open decision still blocks.
#[test]
fn delegated_work_keeps_the_application_working_and_processes_label_its_wait() {
    let (_root, resume, mut session) = setup();
    let delegated = Background {
        delegated: true,
        processes: 0,
    };
    let report = resume.report(session.observation().unwrap(), delegated);
    assert_eq!((report.state, report.background), (State::Working, None));
    let processes = |processes| Background {
        delegated: false,
        processes,
    };
    let report = resume.report(session.observation().unwrap(), processes(1));
    assert_eq!(report.state, State::Idle);
    assert_eq!(report.background.as_deref(), Some("1 process"));
    let report = resume.report(session.observation().unwrap(), processes(3));
    assert_eq!(report.background.as_deref(), Some("3 processes"));
    session.apply(Event::RunStarted {
        conversation: "first".into(),
        run: "run".into(),
        answers: None,
    });
    session.apply(Event::PermissionRequested {
        conversation: "first".into(),
        permission: Permission::new("approval", "coder", "run", "Change?", "Private"),
    });
    assert_eq!(
        resume
            .report(session.observation().unwrap(), delegated)
            .state,
        State::Blocked
    );
}

/// roomkit-rs records a task's end before its result starts the turn that
/// reads it: the last end keeps the application working for `HAND_BACK`,
/// so Herdr does not tell the person it finished in between.
#[test]
fn the_last_task_s_end_keeps_the_application_working_until_its_result_is_read() {
    let start = Instant::now();
    let at = |ms| start + Duration::from_millis(ms);
    let mut delegated = Delegated::default();
    assert!(!delegated.working(0, at(0)));
    assert!(delegated.working(2, at(100)));
    assert!(delegated.working(1, at(5_000)));
    assert!(delegated.working(0, at(6_000)));
    assert!(delegated.working(0, at(6_999)));
    assert!(!delegated.working(0, at(7_000)));
    assert!(delegated.working(1, at(9_000)));
}
