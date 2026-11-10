//! Bounded request diagnostics: shapes and decisions, never provider payloads.
use super::{attempt::Attempt, replay};
use crate::diagnostics::Diagnostics;
use roomkit_ai::{
    AiContentPart, AiContext, AiMessageContent, AiProvider, ProviderError, StreamEvent,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Instant};

pub(super) struct Trace {
    diagnostics: Arc<Diagnostics>,
    fields: Value,
    started: Instant,
    first_event_ms: Option<u128>,
    finished: bool,
}

impl Trace {
    pub(super) fn start(
        diagnostics: Arc<Diagnostics>,
        context: &AiContext,
        model: &str,
        generation: &str,
        attempt: usize,
        budget: Value,
        channel: Option<&str>,
    ) -> Self {
        let known_model = roomkit_anthropic::AnthropicAiProvider::available_models()
            .iter()
            .any(|m| m.id == model);
        let fields = json!({
            "provider":"anthropic", "model":if known_model {model} else {"custom"},
            "generation":generation, "attempt":attempt, "request":uuid::Uuid::new_v4().to_string(),
            "channel":channel, "turn":roomkit::current_turn_host().map(|host| host.turn_id().to_owned()),
        });
        let trace = Self {
            diagnostics,
            fields,
            started: Instant::now(),
            first_event_ms: None,
            finished: false,
        };
        let mut start = shape(context);
        start["max_tokens"] = budget["max_tokens"].clone();
        start["budget"] = budget["budget"].clone();
        start["reasoning_effort"] = json!(match context.reasoning_effort.as_deref() {
            None => "default",
            Some(e @ ("none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max")) => e,
            _ => "custom",
        });
        trace.record("provider_request_started", start);
        trace
    }

    pub(super) fn event(&mut self, event: &Result<StreamEvent, ProviderError>) {
        if event.is_ok() && self.first_event_ms.is_none() {
            self.first_event_ms = Some(self.started.elapsed().as_millis());
        }
    }

    pub(super) fn error(
        &mut self,
        error: &ProviderError,
        details: Value,
        attempt: &Attempt,
        recovery: &str,
    ) {
        let mut fields = details;
        fields["http_status"] = json!(error.status_code);
        fields["retryable"] = json!(error.retryable);
        fields["before_request"] = json!(error.before_request);
        fields["response_started"] = json!(attempt.committed);
        fields["done_received"] = json!(attempt.done.is_some());
        fields["recovery"] = json!(recovery);
        self.finish("error", fields);
    }

    pub(super) fn complete(&mut self, attempt: &Attempt) {
        let reason = attempt
            .done
            .as_ref()
            .and_then(|d| d.finish_reason.as_deref());
        self.finish("completed", json!({
            "finish_reason":match reason {
                Some(r @ ("end_turn" | "stop_sequence" | "tool_use" | "max_tokens" | "pause_turn" | "refusal")) => Some(r),
                Some(_) => Some("other"), None => None,
            },
            "usage":attempt.done.as_ref().map(|d| usage(&d.usage)),
            "response_started":attempt.committed, "done_received":attempt.done.is_some(),
        }));
    }

    pub(super) fn recovery(&self, fields: Value) {
        self.record("provider_recovery", fields);
    }

    fn finish(&mut self, outcome: &str, mut fields: Value) {
        fields["outcome"] = json!(outcome);
        fields["duration_ms"] = json!(self.started.elapsed().as_millis());
        fields["first_event_ms"] = json!(self.first_event_ms);
        self.record("provider_request_finished", fields);
        self.finished = true;
    }

    fn record(&self, kind: &'static str, mut fields: Value) {
        if let (Some(fields), Some(common)) = (fields.as_object_mut(), self.fields.as_object()) {
            fields.extend(common.clone());
        }
        self.diagnostics.record(kind, fields, None);
    }
}

impl Drop for Trace {
    fn drop(&mut self) {
        if !self.finished {
            self.finish("cancelled", json!({}));
        }
    }
}

pub(super) fn usage(metadata: &serde_json::Map<String, Value>) -> Value {
    let mut result = json!({});
    for key in [
        "input_tokens",
        "output_tokens",
        "reasoning_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ] {
        if let Some(count) = metadata.get(key).and_then(Value::as_u64) {
            result[key] = json!(count);
        }
    }
    result
}

fn shape(context: &AiContext) -> Value {
    let (mut thinking, mut calls, mut results, mut text, mut signed, mut redacted) =
        (0, 0, 0, 0, 0, 0);
    for message in &context.messages {
        match &message.content {
            AiMessageContent::Text(_) => text += 1,
            AiMessageContent::Parts(parts) => {
                for part in parts {
                    match part {
                        AiContentPart::Thinking(p) => {
                            thinking += 1;
                            signed +=
                                usize::from(p.signature.as_ref().is_some_and(|s| !s.is_empty()));
                            redacted += usize::from(p.redacted.is_some());
                        }
                        AiContentPart::ToolCall(_) => calls += 1,
                        AiContentPart::ToolResult(_) => results += 1,
                        AiContentPart::Text(_) => text += 1,
                        _ => {}
                    }
                }
            }
        }
    }
    json!({"messages":context.messages.len(), "tools":context.tools.len(),
        "thinking_blocks":thinking, "signed_blocks":signed, "redacted_blocks":redacted,
        "text_blocks":text, "tool_calls":calls, "tool_results":results,
        "restored_tool_inputs":replay::restored_inputs(context),
    })
}

#[cfg(test)]
#[path = "trace_tests.rs"]
mod tests;
