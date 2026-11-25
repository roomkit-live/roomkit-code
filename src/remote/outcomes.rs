//! Report completed worker tools through the accepted turn's owner.
use roomkit::{ToolCallEvent, TurnExecutor};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const REPORT_LIFETIME: std::time::Duration = std::time::Duration::from_secs(86400);
pub(crate) const OUTCOME: &str = "code_observe_workspace_tool";

/// The most bytes of a tool's result, as JSON, a report carries: under the
/// megabyte a hook reads, with room for the rest of its input.
const REPORT_BYTES: usize = 256 * 1024;

/// `value`, or a line saying how large it was when it is past
/// [`REPORT_BYTES`]. The owner's hooks read a report whole: a worker's
/// result of any size would cross the broker, be held by the owner and fail
/// the hooks it was reported for.
fn bounded(value: Option<Value>) -> Option<Value> {
    let value = value?;
    let bytes = serde_json::to_vec(&value).map_or(usize::MAX, |json| json.len());
    Some(if bytes > REPORT_BYTES {
        Value::String(format!(
            "[Tool result omitted from this report: over {} KiB]",
            REPORT_BYTES / 1024
        ))
    } else {
        value
    })
}

#[derive(Deserialize, Serialize)]
pub(crate) struct ToolOutcome {
    result: Option<Value>,
    structured_content: Option<Value>,
    is_error: bool,
    cancelled: bool,
    refused: bool,
}

impl ToolOutcome {
    pub(crate) fn from_owner_action(result: &Result<Value, String>, refused: bool) -> Self {
        Self {
            result: Some(match result {
                Ok(value) => value.clone(),
                Err(error) => Value::String(error.clone()),
            }),
            structured_content: None,
            is_error: result.is_err(),
            cancelled: false,
            refused,
        }
    }

    /// Reports the outcome on `event`. The owner bounds what it is sent as
    /// the worker bounds what it sends: it does not rely on the worker.
    pub(crate) fn apply(self, event: &mut ToolCallEvent) {
        event.result = bounded(self.result);
        event.structured_content = bounded(self.structured_content);
        event.is_error = self.is_error;
        event.cancelled = self.cancelled;
        event.refused = self.refused;
    }
}

/// Match an observer to the exact call for which its coordinator requested a report.
/// An interrupted worker can lose an observer. Expire those entries on the next request.
#[derive(Default)]
pub(super) struct Reports(
    std::sync::Mutex<std::collections::HashMap<(String, String), (std::time::Instant, String)>>,
);

impl Reports {
    pub(super) fn request(&self, turn: &str, call: String, receipt: String) {
        let mut pending = crate::sync::lock(&self.0);
        pending.retain(|_, (at, _)| at.elapsed() < REPORT_LIFETIME);
        pending.insert(
            (turn.to_owned(), call),
            (std::time::Instant::now(), receipt),
        );
    }
}

pub(super) fn install(executor: &TurnExecutor) -> crate::Result<std::sync::Arc<Reports>> {
    let reports = std::sync::Arc::new(Reports::default());
    let observed = reports.clone();
    let options = roomkit::turns::HookOptions {
        execution: roomkit::HookExecution::Async,
        name: "project-tool-outcome".into(),
        timeout: crate::timeouts::HOOKS,
        ..Default::default()
    };
    executor.hook(
        roomkit::HookTrigger::OnToolCall,
        move |mut event: ToolCallEvent, _| {
            let observed = observed.clone();
            async move {
                let Some(host) = roomkit::current_turn_host() else {
                    return;
                };
                let Some((_, receipt)) = crate::sync::lock(&observed.0)
                    .remove(&(host.turn_id().to_owned(), event.tool_call_id.clone()))
                else {
                    return;
                };
                let outcome = ToolOutcome {
                    result: bounded(event.result.take()),
                    structured_content: bounded(event.structured_content.take()),
                    is_error: event.is_error,
                    cancelled: event.cancelled,
                    refused: event.refused,
                };
                let mut fields = serde_json::Map::new();
                fields.insert("receipt".into(), receipt.into());
                fields.insert("outcome".into(), serde_json::json!(outcome));
                if let Err(error) = host.call(OUTCOME, Some(fields)).await {
                    eprintln!("Could not report worker tool outcome to owner: {error}");
                }
            }
        },
        options,
    )?;
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_carries_a_result_within_its_bound_or_says_it_was_omitted() {
        let small = Value::String("ok".into());
        assert_eq!(bounded(Some(small.clone())), Some(small));
        assert_eq!(bounded(None), None);
        let large = Value::String("x".repeat(REPORT_BYTES));
        let reported = bounded(Some(large));
        assert_eq!(
            reported,
            Some(Value::String(
                "[Tool result omitted from this report: over 256 KiB]".into()
            ))
        );
    }
}
