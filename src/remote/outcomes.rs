//! Report completed worker tools through the accepted turn's owner.
use roomkit::{ToolCallEvent, TurnExecutor};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const REPORT_LIFETIME: std::time::Duration = std::time::Duration::from_secs(86400);
pub(crate) const OUTCOME: &str = "code_observe_workspace_tool";

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

    pub(crate) fn apply(self, event: &mut ToolCallEvent) {
        event.result = self.result;
        event.structured_content = self.structured_content;
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
                    result: event.result.take(),
                    structured_content: event.structured_content.take(),
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
