//! The approval card for one tool call: its exact arguments, a command shown
//! as the shell would run it, and a one-line summary for the queue.

use roomkit::{ToolCallEvent, ToolUse};
use roomkit_tui_engine::PermissionRequest;

/// Larger cards are refused rather than cut: the user approves exactly what
/// runs.
const MAX_DETAIL: usize = 65_536;

/// The request for approving `event`, headed by its rule's `label` and by a
/// hook's reason for asking; a refusal when it cannot be shown in full.
pub(super) fn request(
    event: &ToolCallEvent,
    label: &str,
    review: Option<String>,
) -> Result<PermissionRequest, ToolUse> {
    let Ok(arguments) = serde_json::to_string_pretty(&event.call.arguments) else {
        return Err(ToolUse::block("Could not present the action for approval."));
    };
    let detail = command(event, label).unwrap_or_else(|| format!("{label}:\n{arguments}"));
    let detail = if let Some(reason) = review {
        format!("Hook requires review: {reason}\n\n{detail}")
    } else {
        detail
    };
    if detail.len() > MAX_DETAIL {
        return Err(ToolUse::block(
            "The action is too large to review; split it into smaller calls.",
        ));
    }
    Ok(PermissionRequest::new(
        format!("Allow {}?", event.call.name),
        detail,
        crate::timeouts::APPROVAL,
    )
    .with_summary(summary(event)))
}

/// A command as the shell runs it, its other parameters below; control
/// characters other than newlines are shown escaped.
fn command(event: &ToolCallEvent, label: &str) -> Option<String> {
    if !matches!(event.call.name.as_str(), "shell" | "process_start") {
        return None;
    }
    let mut parameters = event.call.arguments.clone();
    let command = parameters.remove("command")?.as_str()?.to_owned();
    let command = if command.chars().any(|c| c.is_control() && c != '\n') {
        serde_json::to_string(&command).unwrap_or_default()
    } else {
        command
    };
    Some(format!(
        "{label}\n\n$ {command}\n\nOther parameters:\n{}",
        serde_json::to_string_pretty(&parameters).unwrap_or_default()
    ))
}

/// The tool and the start of its main target, on one line.
fn summary(event: &ToolCallEvent) -> String {
    let target = ["command", "path", "url", "query", "process_id"]
        .iter()
        .find_map(|key| {
            event
                .call
                .arguments
                .get(*key)
                .and_then(serde_json::Value::as_str)
        });
    let Some(target) = target else {
        return event.call.name.clone();
    };
    let compact = target.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview: String = compact.chars().take(96).collect();
    let ellipsis = if preview.len() < compact.len() {
        "…"
    } else {
        ""
    };
    format!("{} · {preview}{ellipsis}", event.call.name)
}
