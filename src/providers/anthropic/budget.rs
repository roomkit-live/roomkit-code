//! The coding agent's output policy; the library and explicit limits stay unchanged.
use roomkit_ai::{AiContext, AiProvider, memory::estimate_context_tokens};
use roomkit_anthropic::AnthropicAiProvider;

const AUTO: u32 = 32_000;
const GROWN: &str = "roomkit_code_output_budget";

#[derive(Clone)]
pub(super) struct Budget {
    manual: Option<u32>,
    ceiling: u32,
    context: Option<u64>,
}

impl Budget {
    pub(super) fn new(
        provider: &AnthropicAiProvider,
        manual: Option<u32>,
        context: Option<u64>,
    ) -> Self {
        Self {
            manual,
            ceiling: ceiling(provider.model_name()),
            context: context.or_else(|| provider.catalog_entry().and_then(|m| m.context_window)),
        }
    }

    pub(super) fn prepare(&self, context: &mut AiContext) -> bool {
        if context.max_tokens.is_some() || self.manual.is_some() {
            return false;
        }
        let grown = context
            .response_metadata
            .get(GROWN)
            .and_then(|v| v.as_u64());
        let limit = grown.and_then(|n| u32::try_from(n).ok()).unwrap_or(AUTO);
        context.max_tokens = Some(limit.min(self.available(context)));
        grown.is_none()
    }

    pub(super) fn grow(&self, context: &mut AiContext) -> bool {
        let current = context.max_tokens.unwrap_or(AUTO);
        let next = current.saturating_mul(2).min(self.available(context));
        if next <= current {
            return false;
        }
        context.max_tokens = Some(next);
        context.response_metadata.insert(GROWN, next.into());
        true
    }

    fn available(&self, context: &AiContext) -> u32 {
        let available = self.context.map_or(u64::MAX, |window| {
            // Conservative local estimate, with room for tokenizer differences.
            let input = estimate_context_tokens(context) as u64;
            window
                .saturating_sub(input.saturating_mul(115) / 100)
                .max(1)
        });
        self.ceiling
            .min(u32::try_from(available).unwrap_or(u32::MAX))
    }
}

// Conservative ceilings, never an opt-in extended-output beta. Current models
// allow at least 64K; the coding policy spends at most 32K + one 64K retry.
fn ceiling(model: &str) -> u32 {
    if model.starts_with("claude-3-5-") {
        return 8192;
    }
    if model.starts_with("claude-3-") && !model.starts_with("claude-3-7-") {
        return 4096;
    }
    if model.starts_with("claude-2") || model.starts_with("claude-instant") {
        return 4096;
    }
    if model.starts_with("claude-opus-4-1")
        || model.starts_with("claude-opus-4-0")
        || model.starts_with("claude-opus-4-2025")
    {
        return AUTO;
    }
    // Unknown gateway aliases retain a usable default without an assumed higher cap.
    if !roomkit_anthropic::AnthropicAiProvider::available_models()
        .iter()
        .any(|m| m.id == model)
        && !model.starts_with("claude-3-7-")
    {
        return AUTO;
    }
    64_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_is_bounded_and_explicit_limits_are_untouched() {
        let mut context = AiContext::default();
        let budget = Budget {
            manual: None,
            ceiling: 64_000,
            context: Some(10_000),
        };
        assert!(budget.prepare(&mut context));
        assert_eq!(context.max_tokens, Some(10_000));
        assert!(!budget.grow(&mut context));
        for limit in [2000, 8192] {
            let mut context = AiContext {
                max_tokens: Some(limit),
                ..AiContext::default()
            };
            assert!(!budget.prepare(&mut context));
            assert_eq!(context.max_tokens, Some(limit));
        }
        let manual = Budget {
            manual: Some(8192),
            ..budget
        };
        let mut context = AiContext::default();
        assert!(!manual.prepare(&mut context));
        assert_eq!(context.max_tokens, None); // The native provider applies the profile.
    }

    #[test]
    fn known_small_models_and_unknown_aliases_do_not_get_an_unbounded_retry() {
        for (model, limit) in [
            ("claude-3-haiku-20240307", 4096),
            ("claude-3-5-sonnet-latest", 8192),
            ("claude-opus-4-1", 32000),
            ("my-alias", 32000),
            ("claude-fable-5-2", 64000),
        ] {
            assert_eq!(ceiling(model), limit, "{model}");
        }
    }
}
