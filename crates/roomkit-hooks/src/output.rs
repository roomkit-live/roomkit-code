use serde::Deserialize;

use crate::{Event, Report, RunStatus};

/// A decision interpreted before the host's final permission policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Continue normal host policy; never skip its approval or hard restrictions.
    Continue,
    /// Refuse input/action, with the hook's reason.
    Deny(String),
    /// Request human approval, including in an otherwise automatic mode.
    Ask(String),
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    decision: Option<String>,
    reason: Option<String>,
    #[serde(rename = "hookSpecificOutput")]
    specific: Option<Specific>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Specific {
    #[serde(rename = "hookEventName")]
    event: String,
    #[serde(rename = "permissionDecision")]
    decision: String,
    #[serde(default, rename = "permissionDecisionReason")]
    reason: String,
}

/// Interpret only the implemented subset. Invalid output is an error so a
/// blocking caller can fail closed. Observational errors cannot undo an action.
pub fn interpret(event: Event, report: &Report) -> Result<Decision, String> {
    if report.status == RunStatus::Exited(2) && event.blocking() {
        return Ok(Decision::Deny(if report.stderr.trim().is_empty() {
            "Blocked by a configured hook".into()
        } else {
            report.stderr.clone()
        }));
    }
    if report.status != RunStatus::Exited(0) {
        return Err(format!(
            "Hook execution did not succeed ({})",
            report.status.label()
        ));
    }
    if report.stdout.trim().is_empty() {
        return Ok(Decision::Continue);
    }
    let output: Output = serde_json::from_str(&report.stdout).map_err(
        |_| "Hook stdout must be a supported JSON decision or empty; use stderr for logs",
    )?;
    if let Some(specific) = output.specific {
        if event != Event::PreToolUse
            || specific.event != event.name()
            || output.decision.is_some()
            || output.reason.is_some()
        {
            return Err(
                "hookSpecificOutput is only supported for PreToolUse permission decisions".into(),
            );
        }
        return match specific.decision.as_str() {
            "allow" => Ok(Decision::Continue),
            "deny" => Ok(Decision::Deny(specific.reason)),
            "ask" => Ok(Decision::Ask(specific.reason)),
            _ => Err("Unsupported permissionDecision".into()),
        };
    }
    match output.decision.as_deref() {
        None if output.reason.is_none() => Ok(Decision::Continue),
        Some("block") if event == Event::UserPromptSubmit => Ok(Decision::Deny(
            output
                .reason
                .unwrap_or_else(|| "Blocked by a configured hook".into()),
        )),
        _ => Err("Unsupported hook decision for this event".into()),
    }
}
