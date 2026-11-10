//! Coding-session policy over the native provider: safe replay and bounded recovery.
mod attempt;
mod budget;
mod replay;

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
        }
    }

    pub(super) fn generate_structured_stream<'a>(
        &'a self,
        context: &'a AiContext,
    ) -> ProviderStream<'a> {
        Box::pin(async_stream::stream! {
            let mut context = context.clone();
            replay::prepare(&mut context);
            let mut may_grow = self.budget.prepare(&mut context);
            loop {
                let mut stream = self.inner.generate_structured_stream(&context);
                let mut attempt = attempt::Attempt::default();
                while let Some(event) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
                    for event in attempt.accept(event) { yield Ok(event); }
                    if attempt.failed() { break; }
                }
                drop(stream);
                if let Some(error) = attempt.error.take() {
                    if !attempt.committed && attempt.done.is_none() && self.repair(&mut context, &error) {
                        continue;
                    }
                    for event in attempt.finish() { yield Ok(event); }
                    yield Err(error);
                    break;
                }
                if may_grow && attempt.exhausted() && self.grow(&mut context, &attempt) {
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

    fn repair(&self, context: &mut AiContext, error: &ProviderError) -> bool {
        let count = replay::repair(context, error);
        if count == 0 {
            return false;
        }
        self.diagnostics.record(
            "provider_recovery",
            json!({
                "reason":"reasoning_replay", "removed_blocks":count, "http_status":400,
            }),
            None,
        );
        true
    }

    fn grow(&self, context: &mut AiContext, attempt: &attempt::Attempt) -> bool {
        let previous = context.max_tokens;
        if !self.budget.grow(context) {
            return false;
        }
        self.diagnostics.record("provider_recovery", json!({
            "reason":"output_budget", "previous_tokens":previous, "max_tokens":context.max_tokens,
            // The engine does not publish usage if a first tool-less generation
            // ultimately errors. Keep known costs here even if the retry fails.
            "usage":attempt.done.as_ref().map(|done| &done.usage),
        }), None);
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
