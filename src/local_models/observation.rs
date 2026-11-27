//! Explain an incomplete llama.cpp generation before the agent loop folds it into a turn.
use std::sync::Arc;

use roomkit::model::Metadata;
use roomkit_ai::{AiContext, AiResponse, FinishReason, StreamEvent};
use serde_json::{Value, json};

use crate::diagnostics::Diagnostics;

#[derive(Clone)]
pub(super) struct Reporter {
    log: Arc<Diagnostics>,
    profile: String,
    context_size: u32,
    default_budget: u32,
}

/// Only the presence of output is retained, including for partial streams.
#[derive(Default)]
pub(super) struct Output {
    text: bool,
    tool: bool,
    thinking: bool,
}

impl Reporter {
    pub(super) fn new(
        log: Arc<Diagnostics>,
        profile: &str,
        context_size: u32,
        default_budget: u32,
    ) -> Self {
        Self {
            log,
            profile: profile.into(),
            context_size,
            default_budget,
        }
    }

    pub(super) fn whole(&self, request: &AiContext, response: &AiResponse) {
        let output = Output {
            text: !response.content.trim().is_empty(),
            tool: !response.tool_calls.is_empty(),
            thinking: response.thinking.as_ref().is_some_and(|s| !s.is_empty())
                || response
                    .thinking_parts
                    .as_ref()
                    .is_some_and(|p| !p.is_empty()),
        };
        self.finish(request, &output, response.finish_reason, &response.usage);
    }

    pub(super) fn event(&self, request: &AiContext, output: &mut Output, event: &StreamEvent) {
        match event {
            StreamEvent::TextDelta(delta) => output.text |= !delta.text.trim().is_empty(),
            StreamEvent::ToolCall(_) => output.tool = true,
            StreamEvent::ThinkingDelta(delta) => {
                output.thinking |= !delta.thinking.is_empty() || delta.redacted.is_some()
            }
            StreamEvent::Done(done) => {
                self.finish(request, output, done.finish_reason, &done.usage)
            }
            _ => {}
        }
    }

    fn finish(
        &self,
        request: &AiContext,
        output: &Output,
        reason: Option<FinishReason>,
        usage: &Metadata,
    ) {
        let budget = request
            .max_tokens
            .filter(|n| *n > 0)
            .unwrap_or(self.default_budget);
        let written = usage.get("output_tokens").and_then(Value::as_u64);
        let summary = if reason == Some(FinishReason::Length) {
            if written.is_some_and(|n| n >= u64::from(budget)) {
                format!(
                    "llama.cpp reached the output budget of {budget} tokens. Increase Output budget (advanced) in /agents and retry."
                )
            } else {
                format!(
                    "llama.cpp reported a length limit. The requested output budget was {budget} tokens and the context capacity was {} tokens. Check both settings before retrying.",
                    self.context_size
                )
            }
        } else if !output.text && !output.tool {
            let reason = reason.map_or("unreported", FinishReason::as_str);
            let detail = if output.thinking {
                " It produced reasoning only."
            } else {
                ""
            };
            format!(
                "llama.cpp returned no visible text or tool call (finish reason: {reason}).{detail} Retry the request."
            )
        } else {
            return;
        };
        self.log.local_generation(request.room.as_ref().map(|r| r.room.id.as_str()), json!({
            "agent_profile": self.profile, "turn": roomkit::current_turn_id(), "provider": "llamacpp",
            "finish_reason": reason, "output_budget": budget, "context_capacity": self.context_size,
            "output_tokens": written, "reasoning_seen": output.thinking, "text_seen": output.text,
            "tool_call_seen": output.tool,
        }), summary);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roomkit_ai::{StreamDone, StreamThinkingDelta};

    #[test]
    fn local_output_limits_and_empty_answers_are_explained_without_payloads() -> crate::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("diagnostics.jsonl");
        let log = Diagnostics::open(path.clone(), false)?;
        let reporter = Reporter::new(log, "local", 128_000, 1024);
        let request = AiContext {
            system_prompt: Some("private prompt".into()),
            ..Default::default()
        };
        let mut output = Output::default();
        reporter.event(
            &request,
            &mut output,
            &StreamEvent::ThinkingDelta(StreamThinkingDelta::new("private thought")),
        );
        let mut done = StreamDone {
            finish_reason: Some(FinishReason::Length),
            ..Default::default()
        };
        done.usage.insert("output_tokens".into(), 1024.into());
        done.usage.insert("private metadata".into(), true.into());
        reporter.event(&request, &mut output, &StreamEvent::Done(done));

        let request = AiContext {
            max_tokens: Some(4096),
            ..request
        };
        let mut answer = AiResponse::new("private answer");
        reporter.whole(&request, &answer);
        answer.finish_reason = Some(FinishReason::Length);
        reporter.whole(&request, &answer);
        answer.content.clear();
        answer.finish_reason = Some(FinishReason::Stop);
        reporter.whole(&request, &answer);

        let data = std::fs::read_to_string(path)?;
        assert!(!data.contains("private"));
        let records: Vec<Value> = data
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        let records: Vec<_> = records
            .iter()
            .filter(|v| v["event"] == "local_generation_incomplete")
            .collect();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["output_tokens"], 1024);
        assert_eq!(records[0]["reasoning_seen"], true);
        assert_eq!(records[0]["text_seen"], false);
        assert!(
            records[0]["summary"]
                .as_str()
                .is_some_and(|s| s.contains("reached the output budget of 1024"))
        );
        assert_eq!(records[1]["output_budget"], 4096);
        assert!(records[1]["output_tokens"].is_null());
        assert!(
            records[1]["summary"]
                .as_str()
                .is_some_and(|s| s.contains("reported a length limit"))
        );
        assert_eq!(records[2]["finish_reason"], "stop");
        assert!(
            records[2]["summary"]
                .as_str()
                .is_some_and(|s| s.contains("no visible text"))
        );
        Ok(())
    }
}
