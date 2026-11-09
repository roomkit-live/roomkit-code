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
        spenders: Vec::new(),
        tasks_uncounted: 0,
        turn_left_running: false,
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
        spenders: Vec::new(),
        tasks_uncounted: 0,
        turn_left_running: false,
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
        spenders: Vec::new(),
        tasks_uncounted: 0,
        turn_left_running: false,
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

mod delegated {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::delegation::spending::Spent;
    use crate::delegation::{Outcome, Task};

    fn usage(input: u64, output: u64, turns: u64) -> Usage {
        Usage {
            input,
            cached: 0,
            output,
            turns,
            complete: true,
            last_turn_recorded: true,
        }
    }

    fn usd(micros: u64) -> ConversationCost {
        ConversationCost::new(micros, "USD", true)
    }

    fn lead() -> Vec<Spender> {
        let (id, label) = ("code".to_owned(), "Code".to_owned());
        vec![Spender::lead(
            id,
            label,
            usage(1_000, 100, 2),
            Some(usd(420_000)),
        )]
    }

    fn task(agent: &str, spent: Option<Spent>, ended: bool) -> Task {
        Task {
            id: format!("{agent}-{}", spent.is_some()),
            parent: "main".into(),
            room: "main::task".into(),
            agent: agent.into(),
            agent_id: agent.to_lowercase(),
            input: "Review".into(),
            started: Instant::now(),
            ended: ended.then_some((Duration::from_secs(3), Outcome::Completed)),
            spent,
        }
    }

    fn spent(input: u64, output: u64, cost: Option<ConversationCost>) -> Option<Spent> {
        Some(Spent {
            usage: usage(input, output, 1),
            cost,
        })
    }

    fn summary() -> Summary {
        Summary {
            usage: Some(usage(1_000, 100, 2)),
            cost: Some(usd(420_000)),
            ..Summary::default()
        }
    }

    #[test]
    fn the_report_totals_the_conversation_then_gives_each_agent_s_part() {
        let mut summary = summary();
        summary.add_tasks(
            lead(),
            &[
                task("Review", spent(500, 50, Some(usd(300_000))), true),
                task("Docs", spent(200, 20, Some(usd(100_000))), true),
                task("Review", spent(300, 30, Some(usd(180_000))), true),
            ],
            "code",
        );
        assert_eq!(
            summary.conversation_report(),
            [
                "Token usage: total=2200 input=2000 (+ 0 cached) output=200 · 5 turns · cost ≈ $1.00",
                "  Code: total=1100 · 2 turns · ≈ $0.42",
                "  Review (2 tasks): total=880 · 2 turns · ≈ $0.48",
                "  Docs (1 task): total=220 · 1 turn · ≈ $0.10",
            ]
        );
    }

    /// A worker with no price is counted in the tokens, and leaves the cost
    /// partial rather than silently lower.
    #[test]
    fn a_task_with_no_price_leaves_the_total_partial() {
        let mut summary = summary();
        summary.add_tasks(
            lead(),
            &[task("Review", spent(500, 50, None), true)],
            "code",
        );
        let lines = summary.conversation_report();
        assert_eq!(
            lines[0],
            "Token usage: total=1650 input=1500 (+ 0 cached) output=150 · 3 turns · cost ≈ $0.42 (partial)"
        );
        assert_eq!(lines[2], "  Review (1 task): total=550 · 1 turn");
    }

    /// A task that runs is cut by closing and was not counted: the total
    /// says so, and the agent that delegated keeps a whole line.
    #[test]
    fn a_running_task_is_counted_aside_and_leaves_only_the_total_partial() {
        let mut running = summary();
        running.add_tasks(lead(), &[task("Review", None, false)], "code");
        assert_eq!(
            running.conversation_report(),
            [
                "Token usage: total=1100 input=1000 (+ 0 cached) output=100 · 2 turns (partial) · cost ≈ $0.42 (partial)",
                "  Code: total=1100 · 2 turns · ≈ $0.42",
                "  1 delegated task not counted: still running or unreadable",
            ]
        );
    }

    /// A task that ended but could not be read is unknown: the report says
    /// the total lacks something rather than leaving it out in silence.
    #[test]
    fn an_ended_task_that_could_not_be_read_leaves_the_total_partial() {
        let mut unread = summary();
        unread.add_tasks(lead(), &[task("Review", None, true)], "code");
        let lines = unread.conversation_report();
        assert!(
            lines[0].contains("· 2 turns (partial) · cost ≈ $0.42 (partial)"),
            "{lines:?}"
        );
    }

    /// A turn the view left running is the shown agent's: its own line says
    /// it is cut, with the total.
    #[test]
    fn a_cut_turn_marks_its_agent_s_line() {
        let mut cut = summary();
        cut.usage.as_mut().unwrap().last_turn_recorded = false;
        cut.turn_running();
        let done = task("Review", spent(500, 50, Some(usd(1_000))), true);
        cut.add_tasks(lead(), &[done], "code");
        let lines = cut.conversation_report();
        assert_eq!(
            lines[1], "  Code: total=1100 · 2 turns · ≈ $0.42 (partial)",
            "{lines:?}"
        );
        assert!(
            lines[0].contains("(partial) · cost ≈ $0.42 (partial)"),
            "{lines:?}"
        );
    }

    /// The agent shown at exit may have answered nothing yet: the turn cut is
    /// its own, and leaves another agent's line whole.
    #[test]
    fn a_cut_turn_leaves_another_agent_s_line_whole() {
        let mut cut = summary();
        cut.usage.as_mut().unwrap().last_turn_recorded = false;
        cut.turn_running();
        let done = task("Review", spent(500, 50, Some(usd(1_000))), true);
        cut.add_tasks(lead(), &[done], "other");
        let lines = cut.conversation_report();
        assert_eq!(
            lines[1], "  Code: total=1100 · 2 turns · ≈ $0.42",
            "{lines:?}"
        );
        assert!(lines[0].contains("· cost ≈ $0.42 (partial)"), "{lines:?}");
    }

    /// The conversation's first turn, cut after its task ended: the room holds
    /// no turn of its own yet, and the report still cannot be whole.
    #[test]
    fn a_first_turn_cut_after_its_task_ended_leaves_the_report_partial() {
        let mut first = Summary {
            usage: Some(Usage {
                last_turn_recorded: false,
                ..usage(0, 0, 0)
            }),
            cost: Some(usd(0)),
            ..Summary::default()
        };
        first.turn_running();
        let done = task("Review", spent(1_000, 100, Some(usd(3_000))), true);
        first.add_tasks(lead(), &[done], "code");
        let lines = first.conversation_report();
        assert!(lines[0].contains("· 1 turn (partial)"), "{lines:?}");
    }

    /// A room that could not be read gives no agent's part: what is left of
    /// the cost cannot be the whole.
    #[test]
    fn a_room_that_could_not_be_read_leaves_the_total_partial() {
        let mut unread = summary();
        let done = task("Review", spent(500, 50, Some(usd(1_000))), true);
        unread.add_tasks(Vec::new(), &[done], "code");
        let lines = unread.conversation_report();
        assert!(lines[0].ends_with("(partial)"), "{lines:?}");
    }
}
