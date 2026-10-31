//! The approval card for one tool call: its exact arguments, a command shown
//! as the shell would run it, and a one-line summary for the queue.

use roomkit::{ToolCallEvent, ToolUse};
use roomkit_tui_engine::PermissionRequest;

/// Larger cards are refused rather than cut: the user approves exactly what
/// runs.
const MAX_DETAIL: usize = 65_536;

type Arguments = serde_json::Map<String, serde_json::Value>;

/// The request for approving `event`, headed by its rule's `label` and by a
/// hook's reason for asking; a refusal when it cannot be shown in full.
pub(super) fn request(
    event: &ToolCallEvent,
    label: &str,
    review: Option<String>,
) -> Result<PermissionRequest, ToolUse> {
    let (name, arguments) = (event.call.name.as_str(), &event.call.arguments);
    let detail = detail(name, arguments, label, review)?;
    Ok(
        PermissionRequest::new(format!("Allow {name}?"), detail, crate::timeouts::APPROVAL)
            .with_summary(summary(name, arguments)),
    )
}

/// The card's body: the hook's reason, the label and the exact arguments.
fn detail(
    name: &str,
    arguments: &Arguments,
    label: &str,
    review: Option<String>,
) -> Result<String, ToolUse> {
    let Ok(pretty) = serde_json::to_string_pretty(arguments) else {
        return Err(ToolUse::block("Could not present the action for approval."));
    };
    let detail = command(name, arguments, label).unwrap_or_else(|| format!("{label}:\n{pretty}"));
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
    Ok(detail)
}

/// A command as the shell runs it, its other parameters below; control
/// characters other than newlines are shown escaped.
fn command(name: &str, arguments: &Arguments, label: &str) -> Option<String> {
    if !matches!(name, "shell" | "process_start") {
        return None;
    }
    let mut parameters = arguments.clone();
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
fn summary(name: &str, arguments: &Arguments) -> String {
    let target = ["command", "path", "url", "query", "process_id"]
        .iter()
        .find_map(|key| arguments.get(*key).and_then(serde_json::Value::as_str));
    let Some(target) = target else {
        return name.into();
    };
    let compact = target.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview: String = compact.chars().take(96).collect();
    let ellipsis = if preview.len() < compact.len() {
        "…"
    } else {
        ""
    };
    format!("{name} · {preview}{ellipsis}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A refused card fails the test")]
    use super::*;
    use serde_json::json;

    fn arguments(value: serde_json::Value) -> Arguments {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn a_command_is_shown_as_the_shell_runs_it_with_controls_escaped() {
        let plain = arguments(json!({"command": "cargo test\n", "timeout_ms": 1000}));
        let shown = detail("shell", &plain, "Command", None).unwrap();
        assert!(shown.starts_with("Command\n\n$ cargo test\n"), "{shown}");
        assert!(shown.contains("Other parameters:") && shown.contains("timeout_ms"));
        let hidden = arguments(json!({"command": "echo hi\u{1b}[2J"}));
        let shown = detail("process_start", &hidden, "Command", None).unwrap();
        assert!(shown.contains(r#"$ "echo hi\u001b[2J""#), "{shown}");
        // Another tool shows its exact arguments under its label.
        let write = arguments(json!({"path": "a.rs"}));
        let shown = detail("write", &write, "Change", None).unwrap();
        assert!(shown.starts_with("Change:\n{"), "{shown}");
    }

    #[test]
    fn a_hooks_reason_heads_the_card() {
        let shown = detail("read", &Arguments::new(), "Change", Some("audit".into())).unwrap();
        assert!(shown.starts_with("Hook requires review: audit\n\nChange:"));
    }

    #[test]
    fn a_card_too_large_to_show_in_full_is_refused() {
        let large = arguments(json!({"path": "a.rs", "content": "x".repeat(MAX_DETAIL)}));
        assert!(detail("write", &large, "Change", None).is_err());
    }

    #[test]
    fn the_summary_names_the_tool_and_the_start_of_its_target() {
        assert_eq!(summary("diff", &Arguments::new()), "diff");
        let spaced = arguments(json!({"command": "cargo   test\n  --all"}));
        assert_eq!(summary("shell", &spaced), "shell · cargo test --all");
        let long = arguments(json!({"query": "q".repeat(97)}));
        assert_eq!(
            summary("web_search", &long),
            format!("web_search · {}…", "q".repeat(96))
        );
        let exact = arguments(json!({"query": "q".repeat(96)}));
        assert_eq!(
            summary("web_search", &exact),
            format!("web_search · {}", "q".repeat(96))
        );
    }
}
