#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "A broken fixture fails the test"
)]
use super::*;
use roomkit_ai::{AiMessage, AiRole, AiTextPart, ProviderErrorOptions};

fn signed(signature: &str) -> AiContentPart {
    AiContentPart::Thinking(AiThinkingPart {
        signature: Some(signature.into()),
        ..AiThinkingPart::default()
    })
}
fn error(path: &str, status: u16) -> ProviderError {
    ProviderError::new(
        format!("{path}: Invalid `signature` in `thinking` block."),
        ProviderErrorOptions {
            status_code: Some(status),
            ..ProviderErrorOptions::default()
        },
    )
}
fn context() -> AiContext {
    AiContext {
        messages: vec![AiMessage::new(
            AiRole::Assistant,
            vec![
                signed("first"),
                AiContentPart::Thinking(AiThinkingPart::new("unsigned")),
                AiContentPart::Text(AiTextPart::new("Keep text")),
                signed("second"),
                AiContentPart::Thinking(AiThinkingPart {
                    redacted: Some("opaque".into()),
                    ..AiThinkingPart::default()
                }),
            ],
        )],
        ..AiContext::default()
    }
}
#[test]
fn wire_indices_skip_unsigned_thinking_and_discard_only_the_rejected_suffix() {
    let original = context();
    let mut repaired = original.clone();
    assert_eq!(
        repair(&mut repaired, &error("messages.0.content.2", 400)),
        Ok(2)
    );
    assert!(repaired.messages[0].text().contains("Keep text"));
    let AiMessageContent::Parts(parts) = &repaired.messages[0].content else {
        panic!("parts")
    };
    assert!(parts.contains(&signed("first")));
    assert!(!parts.contains(&signed("second")));
    let mut next = original; // The engine still holds earlier rounds.
    prepare(&mut next);
    assert_eq!(next.messages, repaired.messages);
    assert_eq!(
        repair(&mut next, &error("messages.0.content.0", 400)),
        Err("already_repaired")
    );
    let stored = next.response_metadata.get(REMOVED).unwrap().to_string();
    assert!(!stored.contains("second") && !stored.contains("opaque"));
}
#[test]
fn unrelated_status_invalid_path_and_a_path_to_text_never_repair() {
    for (path, status) in [
        ("messages.0.content.0", 500),
        ("messages.4.content.0", 400),
        ("messages.0.content.1", 400),
        ("messages.0.content.99", 400),
        ("messages.no.content.0", 400),
    ] {
        let mut context = context();
        let before = context.messages.clone();
        assert!(repair(&mut context, &error(path, status)).is_err());
        assert_eq!(context.messages, before);
    }
}

#[test]
fn error_paths_address_combined_roles_and_combined_content() {
    let mut context = context();
    context.messages.splice(
        0..0,
        [
            AiMessage::new(AiRole::User, "First instruction"),
            AiMessage::new(AiRole::System, "More instruction"),
            AiMessage::new(AiRole::Tool, "Tool text"),
            AiMessage::new(AiRole::Assistant, "Narration"),
        ],
    );
    assert_eq!(
        repair(&mut context, &error("messages.1.content.3", 400)),
        Ok(2)
    );
    assert_eq!(context.messages[3].text(), "Narration");
    let AiMessageContent::Parts(parts) = &context.messages[4].content else {
        panic!("parts")
    };
    assert!(parts.contains(&signed("first")));
    assert!(!parts.contains(&signed("second")));
}

#[test]
fn diagnostic_paths_distinguish_raw_and_combined_views_without_changing_repair() {
    let mut context = context();
    context
        .messages
        .insert(0, AiMessage::new(AiRole::Assistant, "Narration"));
    let failure = error("messages.0.content.1", 400);
    let fields = error_details(&context, &failure);
    assert_eq!(fields["block_path"], serde_json::json!([0, 1]));
    assert_eq!(fields["grouped_target"], serde_json::json!([1, 0]));
    assert!(fields["raw_target"].is_null());
    assert_eq!(fields["reasoning_error"], "invalid_signature");
    assert_eq!(repair(&mut context, &failure), Ok(4));
}

#[test]
fn failed_repairs_explain_the_decision() {
    for (path, status, reason) in [
        ("messages.0.content.0", 500, "wrong_http_status"),
        ("messages.no.content.0", 400, "missing_block_path"),
        ("messages.0.content.1", 400, "target_not_thinking"),
    ] {
        assert_eq!(repair(&mut context(), &error(path, status)), Err(reason));
    }
}

#[test]
fn prefix_mismatch_resets_reasoning_without_a_local_path_and_never_retries_an_empty_reset() {
    let failure = ProviderError::new(
        "messages.9.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation. Content before this block differs, first at `messages.62.content.0`.",
        ProviderErrorOptions {
            status_code: Some(400),
            ..ProviderErrorOptions::default()
        },
    );
    let mut context = context();
    let details = error_details(&context, &failure);
    assert_eq!(details["mismatch_path"], serde_json::json!([62, 0]));
    assert!(details["grouped_target"].is_null());
    assert_eq!(repair(&mut context, &failure), Ok(4));
    assert_eq!(context.messages[0].text(), "Keep text");
    assert_eq!(repair(&mut context, &failure), Err("already_repaired"));
    assert_eq!(
        repair(&mut AiContext::default(), &failure),
        Err("no_thinking_blocks")
    );
}

#[test]
fn prefix_reset_requires_both_strict_error_phrases_and_http_400() {
    for (message, status) in [
        (
            "messages.9.content.0: Invalid signature in thinking block. The block is bound to a different conversation.",
            500,
        ),
        (
            "messages.9.content.0: Invalid signature in thinking block.",
            400,
        ),
        (
            "messages.9.content.0: The block is bound to a different conversation.",
            400,
        ),
    ] {
        let failure = ProviderError::new(
            message,
            ProviderErrorOptions {
                status_code: Some(status),
                ..ProviderErrorOptions::default()
            },
        );
        let mut context = context();
        let before = context.messages.clone();
        assert!(repair(&mut context, &failure).is_err());
        assert_eq!(context.messages, before);
    }
}

#[test]
fn mismatch_paths_are_numeric_and_truncated_or_unknown_details_stay_absent() {
    assert_eq!(
        mismatch_path("first at messages.12.content.3."),
        Some((12, 3))
    );
    for text in [
        "first at messages.12.content.3",
        "first at messages.12.content.private",
        "first at messages.12.content.3secret",
        "system prompt changed",
    ] {
        assert_eq!(mismatch_path(text), None);
    }
}
