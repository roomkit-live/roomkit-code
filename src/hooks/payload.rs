//! What a hook is told of a tool call, and how text is bounded and shown.
use super::*;

pub(super) fn tool_payload(payload: &mut Value, event: &ToolCallEvent) {
    payload["tool_name"] = json!(event.name);
    payload["tool_use_id"] = json!(event.tool_call_id);
    payload["tool_input"] = json!(event.arguments);
}

/// What a command hook reads of a call's result: a tool's data and text as
/// one JSON text, as it always has, or the result as the model read it.
pub(super) fn tool_response(event: &ToolCallEvent) -> Value {
    let read = event.result.clone().unwrap_or(Value::Null);
    let data = event
        .structured_content
        .as_ref()
        .and_then(|copy| copy.get("data"));
    let (Some(data), Some(text)) = (data, read.as_str()) else {
        return read;
    };
    let mut envelope = json!({ "data": data, "text": text });
    // Nested project instructions reach the model as JSON beside the text.
    if let Ok(Value::Object(mut fields)) = serde_json::from_str::<Value>(text)
        && fields.len() == 2
        && let (Some(text), Some(rules)) =
            (fields.remove("text"), fields.remove("project_instructions"))
    {
        envelope["text"] = text;
        envelope["project_instructions"] = rules;
    }
    Value::String(envelope.to_string())
}

/// How a finished tool call ended, as far as the engine's tool event tells:
/// `served`, `refused`, `cancelled`, or `failed` for any other error. The
/// event carries no outcome of its own, as Python's, so a call a hook
/// withheld or nothing served reads as `failed` too.
pub(crate) fn tool_outcome(event: &ToolCallEvent) -> &'static str {
    if !event.is_error {
        "served"
    } else if event.refused {
        "refused"
    } else if event.cancelled {
        "cancelled"
    } else {
        "failed"
    }
}

/// At most `limit` bytes of `text`, cut at a character boundary, without
/// control characters other than newlines and tabs. Taking `limit`
/// characters first only bounds the copy of a long text: no more can fit.
pub(super) fn bounded(text: &str, limit: usize) -> String {
    let mut text: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(limit)
        .collect();
    text.truncate(text.floor_char_boundary(limit.min(text.len())));
    text
}

/// At most `limit` bytes of a command as it is reviewed: every control
/// character other than a newline or a tab shown as its escape, `\u{1b}`
/// for one, rather than left out. A command read without them is not the
/// command that runs: an escape sequence could hide part of it from the
/// person who enables it.
pub(super) fn reviewed(command: &str, limit: usize) -> String {
    let mut shown = String::new();
    for c in command.chars() {
        if c.is_control() && !matches!(c, '\n' | '\t') {
            shown.extend(c.escape_unicode());
        } else {
            shown.push(c);
        }
        if shown.len() >= limit {
            break;
        }
    }
    shown.truncate(shown.floor_char_boundary(limit.min(shown.len())));
    shown
}
