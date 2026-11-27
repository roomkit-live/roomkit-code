//! Fit recalled material into one allowance shared by an index and explicit lookups.
use roomkit_ai::{
    AiContentPart, AiContext, AiMessage, AiMessageContent, AiRole, Tool,
    memory::{estimate_context_tokens, estimate_message_tokens, estimate_tokens},
};
use roomkit_tools_memory::{MemoryInput, MemoryTool};
use std::{borrow::Cow, collections::HashSet, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(crate) struct View {
    store: MemoryTool,
    window: Option<usize>,
    output: usize,
    allowance: usize,
}
impl View {
    pub fn new(store: MemoryTool, window: Option<u64>, output: Option<u32>) -> Self {
        let window = window.and_then(|v| usize::try_from(v).ok());
        Self {
            store,
            window,
            output: output.unwrap_or_default() as usize,
            allowance: window.map_or(256, |w| (w / 32).min(1024)),
        }
    }

    pub fn tool(&self, history: Option<super::History>) -> impl Tool {
        super::tool::RecallTool {
            history,
            inner: roomkit_tools_roomkit::Memory::new(self.store.clone()),
            allowance: self.allowance,
        }
    }

    async fn titles(&self) -> Vec<String> {
        let input = MemoryInput {
            action: "search".into(),
            ..Default::default()
        };
        let token = CancellationToken::new();
        let _cancel = token.clone().drop_guard();
        let result =
            tokio::time::timeout(Duration::from_secs(1), self.store.call(input, &token)).await;
        match result {
            Ok(Ok(output)) => output
                .data
                .notes
                .into_iter()
                .map(|n| format!("{} — {}", n.id, n.title))
                .collect(),
            // An unavailable index must not block a conversation. An explicit call
            // still explains the backend error to the user.
            _ => Vec::new(),
        }
    }

    pub(super) async fn prepare<'a>(&self, context: &'a AiContext) -> Cow<'a, AiContext> {
        let titles = self.titles().await;
        if titles.is_empty() && !context.messages.iter().any(has_memory_result) {
            return Cow::Borrowed(context);
        }
        Cow::Owned(self.project(context.clone(), &titles))
    }

    fn project(&self, mut context: AiContext, titles: &[String]) -> AiContext {
        let mut results = Vec::new();
        for (i, message) in context.messages.iter_mut().enumerate() {
            if let AiMessageContent::Parts(parts) = &mut message.content {
                for (j, part) in parts.iter_mut().enumerate() {
                    if let AiContentPart::ToolResult(t) = part
                        && t.name == "memory"
                        && !t.is_error
                    {
                        let text = t.as_text();
                        t.result = String::new().into();
                        results.push((i, j, text));
                    }
                }
            }
        }
        let mut remaining = self.allowance;
        if let Some(window) = self.window {
            let reserved = context.max_tokens.map_or(self.output, |v| v as usize);
            remaining = remaining.min(
                window
                    .saturating_sub(estimate_context_tokens(&context))
                    .saturating_sub(reserved),
            );
        }
        let mut seen = HashSet::new();
        for (i, j, text) in results.into_iter().rev() {
            if remaining == 0 || !seen.insert(text.clone()) {
                continue;
            }
            let shown = excerpt(&text, remaining);
            remaining = remaining.saturating_sub(estimate_tokens(&shown));
            if let AiMessageContent::Parts(parts) = &mut context.messages[i].content
                && let AiContentPart::ToolResult(t) = &mut parts[j]
            {
                t.result = shown.into();
            }
        }
        if let Some(index) = index_message(titles, remaining) {
            // Keep the latest user's request and its tool-call/result sequence intact.
            context.messages.insert(0, index);
        }
        context
    }
}

fn has_memory_result(message: &AiMessage) -> bool {
    let AiMessageContent::Parts(parts) = &message.content else {
        return false;
    };
    parts.iter().any(|part| matches!(part, AiContentPart::ToolResult(result) if result.name == "memory" && !result.is_error))
}

fn index_message(titles: &[String], budget: usize) -> Option<AiMessage> {
    use roomkit_core::rules::transcript::without_tags;
    const TAG: &str = "project_memory_index";
    let mut block = format!("<{TAG}>\n");
    let closing = format!("</{TAG}>");
    let envelope = AiMessage::new(AiRole::User, format!("{block}{closing}"));
    // Charge the envelope first. Each complete line costs at least its RoomKit
    // estimate; any discarded rounding space stays free for the conversation.
    let mut space = budget.saturating_sub(estimate_message_tokens(&envelope));
    let mut added = false;
    for title in titles {
        let line = without_tags(TAG, title);
        let cost = estimate_tokens(&line) + 1;
        if cost > space {
            break;
        }
        block.push_str(&line);
        block.push('\n');
        space -= cost;
        added = true;
    }
    block.push_str(&closing);
    added.then(|| AiMessage::new(AiRole::User, block))
}

/// RoomKit estimates one token per four code points, plus one.
pub(super) fn excerpt(text: &str, budget: usize) -> String {
    clip_chars(text, budget.saturating_mul(4).saturating_sub(1))
}

pub(super) fn clip_chars(text: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    if text.chars().count() <= limit {
        return text.into();
    }
    let mut kept: String = text.chars().take(limit.saturating_sub(1)).collect();
    kept.push('…');
    kept
}
