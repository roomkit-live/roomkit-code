#![expect(clippy::unwrap_used, reason = "test assertions")]

use serde_json::json;

use super::*;

fn usage(turns: u64, complete: bool) -> Usage {
    Usage {
        last_turn_recorded: false,
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
        cost: None,
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
        cost: None,
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
        cost: None,
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

fn record(value: &Value) -> Metadata {
    value.as_object().cloned().unwrap_or_default()
}

fn empty() -> Usage {
    Usage {
        input: 0,
        cached: 0,
        output: 0,
        turns: 0,
        complete: true,
        last_turn_recorded: false,
    }
}

#[test]
fn a_stopped_turn_counts_what_it_told() {
    let mut usage = empty();
    usage.read(
        true,
        &record(&json!({
            "ai_usage": { "input_tokens": 3000, "output_tokens": 20 },
            "ai_usage_complete": true,
            "loop_end_reason": "cancelled",
        })),
    );
    assert_eq!((usage.input, usage.output, usage.turns), (3000, 20, 1));
    assert!(usage.complete);
}

#[test]
fn a_turn_with_no_message_counts_the_record_on_its_last_row() {
    let mut usage = empty();
    usage.read(false, &record(&json!({ "tool_name": "shell" })));
    usage.read(
        false,
        &record(&json!({
            "ai_usage": { "input_tokens": 6000, "output_tokens": 30 },
            "ai_usage_complete": true,
            "loop_end_reason": "cancelled",
        })),
    );
    assert_eq!((usage.input, usage.output, usage.turns), (6000, 30, 1));
    assert!(usage.complete && usage.last_turn_recorded);
    // A row after it with no record leaves the turn recorded.
    usage.read(false, &record(&json!({ "tool_name": "shell" })));
    assert!(usage.last_turn_recorded);
}

#[test]
fn a_provider_that_reported_nothing_leaves_the_total_partial() {
    let mut usage = empty();
    usage.read(
        true,
        &record(&json!({
            "ai_usage": {},
            "ai_usage_complete": false,
            "loop_end_reason": "completed",
        })),
    );
    assert!(!usage.complete);
}

#[test]
fn a_turn_stopped_before_anything_told_leaves_the_total_partial() {
    let mut usage = empty();
    usage.read(true, &record(&json!({ "loop_end_reason": "cancelled" })));
    assert_eq!(usage.turns, 0);
    assert!(!usage.complete);
}

#[test]
fn a_turn_told_nothing_still_prints_its_partial_total() {
    let summary = Summary {
        usage: Some(usage(0, false)),
        cost: None,
        ..Summary::default()
    };
    assert!(
        summary
            .lines(false)
            .iter()
            .any(|line| line.contains("(partial)"))
    );
    let idle = Summary {
        usage: Some(usage(0, true)),
        cost: None,
        ..Summary::default()
    };
    assert!(
        !idle
            .lines(false)
            .iter()
            .any(|line| line.starts_with("Token usage"))
    );
}

#[test]
fn a_run_with_no_turn_counted_leaves_the_report_as_it_is() {
    let mut summary = Summary {
        usage: Some(empty()),
        ..Summary::default()
    };
    summary.turn_running();
    assert!(summary.usage.as_ref().unwrap().complete);
}

#[test]
fn a_turn_left_running_leaves_the_total_partial_until_its_record_is_written() {
    let mut summary = Summary {
        usage: Some(empty()),
        ..Summary::default()
    };
    let total = summary.usage.as_mut().unwrap();
    total.read(
        true,
        &record(&json!({
            "ai_usage": { "input_tokens": 1000, "output_tokens": 10 },
            "ai_usage_complete": true,
            "loop_end_reason": "completed",
        })),
    );
    // A `!` command after it starts no turn.
    total.read(true, &record(&json!({ "shell_command": "sleep 300" })));
    summary.turn_running();
    assert!(summary.usage.as_ref().unwrap().complete);

    let total = summary.usage.as_mut().unwrap();
    total.read(true, &record(&json!({ "body": "Second" })));
    summary.turn_running();
    assert!(!summary.usage.as_ref().unwrap().complete);

    let mut summary = Summary {
        usage: Some(empty()),
        ..Summary::default()
    };
    let total = summary.usage.as_mut().unwrap();
    total.read(
        true,
        &record(&json!({
            "ai_usage": { "input_tokens": 3000, "output_tokens": 20 },
            "ai_usage_complete": true,
            "loop_end_reason": "completed",
        })),
    );
    summary.turn_running();
    assert!(summary.usage.as_ref().unwrap().complete);
}

#[test]
fn an_unattended_launch_reports_the_processes_it_left_running() {
    let summary = Summary {
        session: None,
        resume: None,
        usage: None,
        cost: None,
        running: vec![process("p2", "python3 server.py", 61)],
    };
    assert_eq!(
        summary.process_report(super::Fate::LeftRunning),
        [
            "Left 1 background process running:",
            "  p2 · python3 server.py · 1m 01s"
        ]
    );
}
