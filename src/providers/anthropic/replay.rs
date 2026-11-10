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

pub(super) fn repair(
    context: &mut AiContext,
    error: &ProviderError,
) -> Result<usize, &'static str> {
    // One repair per turn; the hashes survive the engine's next context rebuild.
    if !removed(context).is_empty() {
        return Err("already_repaired");
    }
    let (message, index) = if prefix_mismatch(error) {
        // Anthropic can report indices in an internally folded tool turn.
        // Do not guess which local block it means: reset the reasoning only.
        // Text, calls and results stay intact, including executed side effects.
        (0, 0)
    } else {
        let (message, block) = rejected_block(error)?;
        thinking_location(context, message, block).ok_or("target_not_thinking")?
    };
    let removed = remove_suffix(context, message, index);
    let count = removed.len();
    if count == 0 {
        return Err("no_thinking_blocks");
    }
    context.response_metadata.insert(
        REMOVED,
        Value::Array(removed.into_iter().map(Value::String).collect()),
    );
    remove_empty(context);
    Ok(count)
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

pub(super) fn prefix_mismatch(error: &ProviderError) -> bool {
    let text = error.message.replace('`', "");
    error.status_code == Some(400)
        && text.contains("Invalid signature in thinking block")
        && text.contains("The block is bound to a different conversation.")
}

fn rejected_block(error: &ProviderError) -> Result<(usize, usize), &'static str> {
    if error.status_code != Some(400) {
        return Err("wrong_http_status");
    }
    let text = error.message.replace('`', "");
    if !text
        .split_once(':')
        .is_some_and(|(_, explanation)| explanation.contains("Invalid signature in thinking block"))
    {
        return Err("unsupported_error");
    }
    block_path(&text).ok_or("missing_block_path")
}

fn block_path(text: &str) -> Option<(usize, usize)> {
    let (path, _) = text.split_once(':')?;
    let (message, block) = path
        .trim()
        .strip_prefix("messages.")?
        .split_once(".content.")?;
    Some((message.parse().ok()?, block.parse().ok()?))
}

pub(super) fn error_details(context: &AiContext, error: &ProviderError) -> Value {
    let text = error.message.replace('`', "");
    let lower = text.to_ascii_lowercase();
    let kind = if lower.contains("bound to a different conversation") {
        "prefix_mismatch"
    } else if lower.contains("thinking") && lower.contains("modified") {
        "thinking_modified"
    } else if text.contains("Invalid signature in thinking block") {
        "invalid_signature"
    } else if lower.contains("thinking") || lower.contains("reasoning") {
        "other"
    } else {
        "none"
    };
    let path = block_path(&text);
    serde_json::json!({
        "reasoning_error":kind, "block_path":path,
        "mismatch_path":mismatch_path(&text),
        "grouped_target":path.and_then(|(m,b)| thinking_location(context,m,b)),
        // Diagnostic only: do not guess when validation stages address different views.
        "raw_target":path.and_then(|(m,b)| raw_location(context,m,b)),
    })
}

fn mismatch_path(text: &str) -> Option<(usize, usize)> {
    // Diagnostic only. The API's explanatory wording is not a retry contract.
    let rest = text.split_once("first at messages.")?.1;
    let (message, rest) = rest.split_once(".content.")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    // A provider message can be truncated; do not report half an index.
    if !rest.get(digits.len()..)?.starts_with(['.', ',', ' ']) {
        return None;
    }
    Some((message.parse().ok()?, digits.parse().ok()?))
}

fn raw_location(context: &AiContext, message: usize, block: usize) -> Option<(usize, usize)> {
    let turn = context.messages.get(message)?;
    if turn.role != AiRole::Assistant {
        return None;
    }
    let AiMessageContent::Parts(parts) = &turn.content else {
        return None;
    };
    let (index, part) = parts
        .iter()
        .enumerate()
        .filter(|(_, p)| serialized(p))
        .nth(block)?;
    matches!(part, AiContentPart::Thinking(_)).then_some((message, index))
}

pub(super) fn restored_inputs(context: &AiContext) -> usize {
    context.messages.iter().filter_map(|m| match &m.content {
        AiMessageContent::Parts(p) => Some(p), _ => None,
    }).flatten().filter(|p| matches!(p, AiContentPart::ToolCall(c) if c.metadata.get(INPUT).is_some_and(Value::is_object))).count()
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
#[path = "replay_tests.rs"]
mod tests;
