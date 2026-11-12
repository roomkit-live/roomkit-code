#![allow(clippy::unwrap_used)]
use super::*;
use serde_json::json;
use std::time::Duration;

fn hook(command: &str) -> CommandHook {
    CommandHook {
        kind: "command".into(),
        command: command.into(),
        timeout: 1,
        status_message: None,
        roomkit: Options::default(),
    }
}

#[test]
fn unsupported_semantics_and_unbounded_configuration_are_rejected() {
    for value in [
        json!({"hooks":{"Stop":[]}}),
        json!({"hooks":{"PreToolUse":[{"hooks":[{"type":"http","command":"echo test"}]}]}}),
        json!({"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"true","async":true}]}]}}),
        json!({"hooks":{"TurnStart":[{"matcher":"shell","hooks":[{"type":"command","command":"true"}]}]}}),
        json!({"hooks":{"PreToolUse":[{"matcher":"[","hooks":[{"type":"command","command":"true"}]}]}}),
        json!({"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"true","timeout":u64::MAX}]}]}}),
    ] {
        assert!(
            Configuration::parse(value.to_string().as_bytes()).is_err(),
            "{value}"
        );
    }
    let config = Configuration::parse(br#"{"hooks":{"PreToolUse":[{"matcher":"^(read|mcp__fixture__.*)$","hooks":[{"type":"command","command":"true"}]}]}}"#).unwrap();
    let group = &config.hooks[&Event::PreToolUse][0];
    assert!(group.matches("mcp__fixture__lookup"));
    assert!(group.matches("read"));
    assert!(!group.matches("write"));
}

#[test]
fn permission_output_never_silently_accepts_unsupported_rewrites() {
    let report = |value: serde_json::Value| Report {
        status: RunStatus::Exited(0),
        stdout: value.to_string(),
        stderr: String::new(),
        elapsed: Duration::ZERO,
    };
    assert_eq!(interpret(Event::PreToolUse, &report(json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask","permissionDecisionReason":"Review changes"}}))).unwrap(), Decision::Ask("Review changes".into()));
    assert!(interpret(Event::PreToolUse, &report(json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{"command":"different"}}}))).is_err());
    assert!(
        interpret(
            Event::TurnEnd,
            &report(json!({"decision":"block","reason":"retry"}))
        )
        .is_err()
    );
    assert_eq!(
        interpret(
            Event::UserPromptSubmit,
            &report(json!({"decision":"block","reason":"Missing issue"}))
        )
        .unwrap(),
        Decision::Deny("Missing issue".into())
    );
}

#[tokio::test]
async fn command_receives_literal_json_without_shell_interpolation() {
    let root = tempfile::tempdir().unwrap();
    let report = run(&hook("cat > received.json; printf '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"Fixture refused\"}}'"), root.path(), &json!({"prompt":"$(touch injected); `touch another`", "hook_event_name":"PreToolUse"})).await;
    assert_eq!(report.status, RunStatus::Exited(0), "{report:?}");
    let received: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("received.json")).unwrap()).unwrap();
    assert_eq!(received["prompt"], "$(touch injected); `touch another`");
    assert!(!root.path().join("injected").exists());
    assert_eq!(
        interpret(Event::PreToolUse, &report).unwrap(),
        Decision::Deny("Fixture refused".into())
    );
}

#[tokio::test]
async fn captures_both_streams_without_deadlock_and_enforces_bounds() {
    let root = tempfile::tempdir().unwrap();
    let report = run(&hook("cat >/dev/null; python3 -c 'import sys; sys.stdout.write(\"x\"*100000); sys.stderr.write(\"y\"*100000)'"), root.path(), &json!({"prompt":"z".repeat(100000)})).await;
    assert_eq!(report.status, RunStatus::OutputLimit);
    assert!(report.stdout.len() <= 32768 && report.stderr.len() <= 32768);
    let report = run(
        &hook("cat >/dev/null; printf 'no permission' >&2; exit 2"),
        root.path(),
        &json!({}),
    )
    .await;
    assert_eq!(
        interpret(Event::PreToolUse, &report).unwrap(),
        Decision::Deny("no permission".into())
    );
    assert!(interpret(Event::PostToolUse, &report).is_err());
}

#[tokio::test]
async fn timeout_and_future_drop_stop_descendants_and_normal_exit_cleans_background_jobs() {
    let root = tempfile::tempdir().unwrap();
    let command = "cat >/dev/null; (sleep 2; touch leaked) & wait";
    let report = run(&hook(command), root.path(), &json!({})).await;
    assert_eq!(report.status, RunStatus::TimedOut);
    let cancelled = tokio::time::timeout(
        Duration::from_millis(100),
        run(&hook(command), root.path(), &json!({})),
    )
    .await;
    assert!(cancelled.is_err());
    let report = run(
        &hook("(sleep 2; touch leaked) & exit 0"),
        root.path(),
        &json!({}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(!root.path().join("leaked").exists(), "{report:?}");
    assert_eq!(report.status, RunStatus::Exited(0), "{report:?}");
}
