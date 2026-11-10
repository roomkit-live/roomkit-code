#![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
use super::*;
use roomkit_ai::{AiMessage, AiRole, StreamDone, StreamTextDelta};

#[test]
fn cancelled_attempts_are_paired_without_logging_private_context() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("diagnostics.jsonl");
    let log = Diagnostics::open(path.clone(), false).unwrap();
    let context = AiContext {
        messages: vec![AiMessage::new(AiRole::User, "PRIVATE_PROMPT")],
        system_prompt: Some("PRIVATE_SYSTEM".into()),
        reasoning_effort: Some("PRIVATE_EFFORT".into()),
        ..AiContext::default()
    };
    let mut trace = Trace::start(
        log,
        &context,
        "PRIVATE_MODEL",
        "generation",
        1,
        json!({"max_tokens":32000, "budget":"auto"}),
        None,
    );
    trace.event(&Ok(StreamEvent::TextDelta(StreamTextDelta::new(
        "PRIVATE_OUTPUT",
    ))));
    drop(trace);
    let text = std::fs::read_to_string(path).unwrap();
    assert!(!text.contains("PRIVATE_"));
    let records = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records[1]["request"], records[2]["request"]);
    assert_eq!(records[1]["model"], "custom");
    assert_eq!(records[2]["outcome"], "cancelled");
    assert!(records[2]["first_event_ms"].is_number());
}

#[test]
fn completed_usage_is_allowlisted_and_drop_does_not_log_twice() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("diagnostics.jsonl");
    let log = Diagnostics::open(path.clone(), false).unwrap();
    let mut trace = Trace::start(
        log,
        &AiContext::default(),
        "claude-fable-5-2",
        "generation",
        1,
        json!({}),
        None,
    );
    let mut attempt = Attempt::default();
    attempt.accept(Ok(StreamEvent::Done(StreamDone {
        finish_reason: Some("PRIVATE_REASON".into()),
        usage: serde_json::from_value(
            json!({"input_tokens":12, "output_tokens":"PRIVATE_VALUE", "PRIVATE_KEY":99}),
        )
        .unwrap(),
        ..StreamDone::default()
    })));
    trace.complete(&attempt);
    drop(trace);
    let text = std::fs::read_to_string(path).unwrap();
    assert!(!text.contains("PRIVATE_"));
    let records = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3);
    assert_eq!(records[2]["outcome"], "completed");
    assert_eq!(records[2]["finish_reason"], "other");
    assert_eq!(records[2]["usage"], json!({"input_tokens":12}));
}
