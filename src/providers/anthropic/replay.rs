//! Keep signed input intact and leave rejected reasoning out for the rest of a turn.
use roomkit_ai::{
    AiContentPart, AiContext, AiMessageContent, AiRole, AiThinkingPart, ProviderError,
    StreamToolCall,
};
use serde_json::Value;
use std::collections::HashSet;

const INPUT: &str = "roomkit_code_anthropic_input";
const REMOVED: &str = "roomkit_code_removed_thinking";

pub(super) fn keep_input(call: &mut StreamToolCall) {
    // The engine normalizes the human-readable intent before executing tools.
    // A signed prefix must replay the original arguments, including that intent.
    call.metadata
        .insert(INPUT.into(), Value::Object(call.arguments.clone()));
}

pub(super) fn prepare(context: &mut AiContext) {
    let removed = removed(context);
    for message in &mut context.messages {
        if let AiMessageContent::Parts(parts) = &mut message.content {
            parts.retain(|part| !matches!(part, AiContentPart::Thinking(p) if removed.contains(&fingerprint(p))));
            for part in parts {
                if let AiContentPart::ToolCall(call) = part
                    && let Some(original) = call.metadata.get(INPUT).and_then(Value::as_object)
                {
                    call.arguments = original.clone();
                }
            }
        }
    }
    remove_empty(context);
}

pub(super) fn repair(context: &mut AiContext, error: &ProviderError) -> usize {
    // One repair per turn; the hashes survive the engine's next context rebuild.
    if !removed(context).is_empty() {
        return 0;
    }
    let Some((message, block)) = rejected_block(error) else {
        return 0;
    };
    let Some((message, index)) = thinking_location(context, message, block) else {
        return 0;
    };
    let removed = remove_suffix(context, message, index);
    let count = removed.len();
    context.response_metadata.insert(
        REMOVED,
        Value::Array(removed.into_iter().map(Value::String).collect()),
    );
    remove_empty(context);
    count
}

fn thinking_location(context: &AiContext, message: usize, block: usize) -> Option<(usize, usize)> {
    // Error paths address the API's combined turns, not the submitted array.
    // System and tool roles both serialize as user in the native provider.
    let mut group = 0;
    let mut previous = None;
    let mut offset = 0;
    for (index, turn) in context.messages.iter().enumerate() {
        let assistant = turn.role == AiRole::Assistant;
        if previous.is_some_and(|role| role != assistant) {
            group += 1;
            offset = 0;
        }
        previous = Some(assistant);
        if group != message {
            continue;
        }
        if !assistant {
            return None;
        }
        match &turn.content {
            AiMessageContent::Text(_) => offset += 1,
            AiMessageContent::Parts(parts) => {
                for (part, value) in parts.iter().enumerate().filter(|(_, p)| serialized(p)) {
                    if offset == block {
                        return matches!(value, AiContentPart::Thinking(_))
                            .then_some((index, part));
                    }
                    offset += 1;
                }
            }
        }
    }
    None
}

fn remove_suffix(context: &mut AiContext, message_index: usize, index: usize) -> HashSet<String> {
    let mut removed = HashSet::new();
    for (position, message) in context.messages.iter_mut().enumerate().skip(message_index) {
        if let AiMessageContent::Parts(parts) = &mut message.content {
            let first = if position == message_index { index } else { 0 };
            let mut at = 0;
            parts.retain(|part| {
                let discard = at >= first && matches!(part, AiContentPart::Thinking(_));
                at += 1;
                if discard && let AiContentPart::Thinking(part) = part {
                    removed.insert(fingerprint(part));
                }
                !discard
            });
        }
    }
    removed
}

fn rejected_block(error: &ProviderError) -> Option<(usize, usize)> {
    if error.status_code != Some(400) {
        return None;
    }
    let text = error.message.replace('`', "");
    let (path, explanation) = text.split_once(':')?;
    if !explanation.contains("Invalid signature in thinking block") {
        return None;
    }
    let (message, block) = path
        .trim()
        .strip_prefix("messages.")?
        .split_once(".content.")?;
    Some((message.parse().ok()?, block.parse().ok()?))
}

fn serialized(part: &AiContentPart) -> bool {
    !matches!(part, AiContentPart::Thinking(p) if p.redacted.is_none() && p.signature.as_ref().is_none_or(String::is_empty))
}

fn fingerprint(part: &AiThinkingPart) -> String {
    crate::digest::sha256_hex(format!("{:?}\0{:?}", part.signature, part.redacted))
}

fn removed(context: &AiContext) -> HashSet<String> {
    context
        .response_metadata
        .get(REMOVED)
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

fn remove_empty(context: &mut AiContext) {
    context
        .messages
        .retain(|m| !matches!(&m.content, AiMessageContent::Parts(p) if p.is_empty()));
}

#[cfg(test)]
mod tests {
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
            format!(
                "{path}: Invalid `signature` in `thinking` block. The block is bound to a different conversation."
            ),
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
            2
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
        assert_eq!(repair(&mut next, &error("messages.0.content.0", 400)), 0);
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
            assert_eq!(repair(&mut context, &error(path, status)), 0);
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
        assert_eq!(repair(&mut context, &error("messages.1.content.3", 400)), 2);
        assert_eq!(context.messages[3].text(), "Narration");
        let AiMessageContent::Parts(parts) = &context.messages[4].content else {
            panic!("parts")
        };
        assert!(parts.contains(&signed("first")));
        assert!(!parts.contains(&signed("second")));
    }
}
