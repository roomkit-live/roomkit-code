//! Coding-session policy over the native provider: safe replay and bounded recovery.
mod attempt;
mod budget;
mod replay;
mod trace;

use crate::diagnostics::Diagnostics;
use roomkit_ai::{
    AiContext, AiProvider, AiResponse, ProviderError, ProviderStream, StreamEvent, TextStream,
    ThinkingBlocks,
};
use roomkit_anthropic::AnthropicAiProvider;
use serde_json::json;
use std::{future::poll_fn, sync::Arc};

#[derive(Clone)]
pub(crate) struct Anthropic {
    pub(super) inner: AnthropicAiProvider,
    budget: budget::Budget,
    diagnostics: Arc<Diagnostics>,
    channel: Option<String>,
}

impl Anthropic {
    pub(crate) fn new(
        inner: AnthropicAiProvider,
        manual: Option<u32>,
        context: Option<u64>,
        diagnostics: Arc<Diagnostics>,
    ) -> Self {
        let budget = budget::Budget::new(&inner, manual, context);
        Self {
            inner,
            budget,
            diagnostics,
            channel: None,
        }
    }

    pub(crate) fn for_agent(mut self, channel: &str) -> Self {
        self.channel = Some(channel.to_owned());
        self
    }

    pub(super) fn generate_structured_stream<'a>(
        &'a self,
        context: &'a AiContext,
    ) -> ProviderStream<'a> {
        Box::pin(async_stream::stream! {
            let mut context = context.clone();
            replay::prepare(&mut context);
            let fixed = self.budget.fixed(&context);
            let generation = uuid::Uuid::new_v4().to_string();
            let mut sequence = 0;
            let mut may_grow = self.budget.prepare(&mut context);
            loop {
                sequence += 1;
                let mut trace = trace::Trace::start(self.diagnostics.clone(), &context,
                    self.inner.model_name(), &generation, sequence, self.budget.diagnostic(&context, fixed), self.channel.as_deref());
                let mut stream = self.inner.generate_structured_stream(&context);
                let mut attempt = attempt::Attempt::default();
                while let Some(event) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
                    trace.event(&event);
                    for event in attempt.accept(event) { yield Ok(event); }
                    if attempt.failed() { break; }
                }
                drop(stream);
                if let Some(error) = attempt.error.take() {
                    if self.repair(&mut context, &error, &attempt, &mut trace) {
                        continue;
                    }
                    for event in attempt.finish() { yield Ok(event); }
                    yield Err(error);
                    break;
                }
                trace.complete(&attempt);
                if may_grow && attempt.exhausted() && self.grow(&mut context, &attempt, &trace) {
                    may_grow = false;
                    // Keep the first request's usage as its own report. Its
                    // reasoning was cut and must not merge with the next one.
                    if let Some(done) = attempt.done.take() { yield Ok(StreamEvent::Done(done)); }
                    continue;
                }
                for event in attempt.finish() { yield Ok(event); }
                break;
            }
        })
    }

    fn repair(
        &self,
        context: &mut AiContext,
        error: &ProviderError,
        attempt: &attempt::Attempt,
        trace: &mut trace::Trace,
    ) -> bool {
        let details = replay::error_details(context, error);
        let result = if attempt.committed {
            Err("response_started")
        } else if attempt.done.is_some() {
            Err("generation_completed")
        } else {
            replay::repair(context, error)
        };
        trace.error(
            error,
            details,
            attempt,
            result.as_ref().err().copied().unwrap_or("retry"),
        );
        if let Ok(count) = result {
            trace.recovery(json!({
                "reason":"reasoning_replay", "removed_blocks":count, "http_status":400,
            }));
            return true;
        }
        false
    }

    fn grow(
        &self,
        context: &mut AiContext,
        attempt: &attempt::Attempt,
        trace: &trace::Trace,
    ) -> bool {
        let previous = context.max_tokens;
        if !self.budget.grow(context) {
            return false;
        }
        trace.recovery(json!({
            "reason":"output_budget", "previous_tokens":previous, "max_tokens":context.max_tokens,
            // The engine does not publish usage if a first tool-less generation
            // ultimately errors. Keep known costs here even if the retry fails.
            "usage":attempt.done.as_ref().map(|done| trace::usage(&done.usage)),
        }));
        true
    }

    pub(super) fn generate_stream<'a>(&'a self, context: &'a AiContext) -> TextStream<'a> {
        Box::pin(async_stream::stream! {
            let mut stream = self.generate_structured_stream(context);
            while let Some(event) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
                match event {
                    Ok(StreamEvent::TextDelta(delta)) => yield Ok(delta.text),
                    Err(error) => { yield Err(error); break; }
                    _ => {}
                }
            }
        })
    }

    pub(super) async fn generate(&self, context: &AiContext) -> Result<AiResponse, ProviderError> {
        let mut stream = self.generate_structured_stream(context);
        let mut response = AiResponse::new("");
        let mut thinking = ThinkingBlocks::new();
        while let Some(event) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
            match event? {
                StreamEvent::TextDelta(delta) => response.content.push_str(&delta.text),
                StreamEvent::ThinkingDelta(delta) => {
                    thinking.add(&delta);
                }
                StreamEvent::ToolCall(call) => response.tool_calls.push(call.into()),
                StreamEvent::ToolCallDelta(_) => {}
                StreamEvent::Done(done) => {
                    for (key, value) in done.usage {
                        if let Some(count) = value.as_u64() {
                            let total = response
                                .usage
                                .get(&key)
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            response
                                .usage
                                .insert(key, total.saturating_add(count).into());
                        }
                    }
                    response.finish_reason = done.finish_reason;
                    response.metadata = done.metadata;
                }
            }
        }
        response.thinking = thinking.seen().then(|| thinking.text());
        response.thinking_signature = thinking.last_signature().map(str::to_owned);
        response.thinking_parts = Some(thinking.parts());
        Ok(response)
    }
}
