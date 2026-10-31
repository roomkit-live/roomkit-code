#![expect(clippy::unwrap_used, reason = "test assertions")]

use super::*;

fn usage(turns: u64, complete: bool) -> Usage {
    Usage {
        input: 300,
        cached: 600,
        output: 90,
        turns,
        complete,
    }
}

fn process(id: &str, command: &str, seconds: u64) -> Process {
    Process {
        id: id.into(),
        command: command.into(),
        elapsed: Duration::from_secs(seconds),
    }
}

#[test]
fn a_resumable_session_reports_its_command_usage_and_stopped_processes() {
    let summary = Summary {
        session: Some("main".into()),
        resume: Some(
            [
                "roomkit-code",
                "/work/my project",
                "--session",
                "main",
                "--mode",
                "ask",
            ]
            .map(String::from)
            .to_vec(),
        ),
        usage: Some(usage(2, true)),
        running: vec![process("proc-1", "npm run\tdev\n", 252)],
    };
    assert_eq!(
        summary.lines(true),
        [
            "Session: main",
            "To continue this session, run:",
            "  roomkit-code '/work/my project' --session main --mode ask",
            "Token usage: total=390 input=300 (+ 600 cached) output=90 · 2 turns",
            "Stopped 1 background process:",
            "  proc-1 · npm run dev · 4m 12s",
        ]
    );
}

#[test]
fn partial_usage_is_marked_and_unconfirmed_cleanup_is_not_reported_as_stopped() {
    let summary = Summary {
        session: Some("main".into()),
        resume: None,
        usage: Some(usage(1, false)),
        running: (0..10)
            .map(|i| process(&format!("p{i}"), "sleep 9", 3700))
            .collect(),
    };
    let lines = summary.lines(false);
    assert_eq!(
        lines[1],
        "Token usage: total=390 input=300 (+ 600 cached) output=90 · 1 turn (partial)"
    );
    assert_eq!(lines[2], "10 background processes may still be running:");
    assert_eq!(lines[3], "  p0 · sleep 9 · 1h 01m");
    assert_eq!(lines.len(), 3 + LISTED + 1);
    assert_eq!(lines.last().unwrap(), "  … 2 more");
}

#[test]
fn an_untouched_draft_without_processes_prints_nothing() {
    assert!(Summary::default().lines(true).is_empty());
    let unreported = Summary {
        session: Some("main".into()),
        usage: Some(usage(0, true)),
        ..Summary::default()
    };
    assert_eq!(unreported.lines(true), ["Session: main"]);
}

#[test]
fn shell_quoting_and_command_lines_are_safe_to_copy() {
    assert_eq!(quoted("--state-dir=/a/b_c.d"), "--state-dir=/a/b_c.d");
    assert_eq!(quoted("it's"), r"'it'\''s'");
    assert_eq!(quoted(""), "''");
    assert_eq!(one_line("a\u{1b}[31mb"), "a [31mb");
    assert_eq!(one_line(&"x".repeat(80)).chars().count(), 60);
    assert_eq!(elapsed(Duration::from_secs(9)), "9s");
}
