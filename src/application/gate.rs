//! The permission gate every tool call passes: Plan's restrictions first,
//! then the command hooks, then the application's policy and approval.

use std::sync::Arc;

use roomkit::{RoomKit, ToolCallEvent, ToolUse};
use roomkit_sqlite::SqliteStore;
use roomkit_tui_engine::ConversationChannel;

use crate::{Result, hooks::Hooks, permissions::Policy};

pub(super) fn install(
    kit: &RoomKit<SqliteStore>,
    policy: &Policy,
    channel: &ConversationChannel,
    hooks: &Arc<Hooks>,
) -> Result<()> {
    let (policy, channel, hooks) = (policy.clone(), channel.clone(), hooks.clone());
    kit.before_tool_use(
        roomkit::ToolHookSpec::new("code-permissions")
            .with_timeout(crate::timeouts::PERMISSION_GATE),
        move |event, _| {
            let (policy, channel, hooks) = (policy.clone(), channel.clone(), hooks.clone());
            async move { Ok(decide(&policy, &channel, &hooks, event).await) }
        },
    )?;
    Ok(())
}

/// Plan refuses before any hook runs; a hook's refusal stands, and its
/// request for review reaches the approval card.
async fn decide(
    policy: &Policy,
    channel: &ConversationChannel,
    hooks: &Hooks,
    event: ToolCallEvent,
) -> ToolUse {
    if policy.plan_blocks(&event) {
        return policy.gate(channel, event).await;
    }
    match hooks.before_tool(&event).await {
        roomkit_hooks::Decision::Deny(reason) => ToolUse::block(reason),
        roomkit_hooks::Decision::Ask(reason) => {
            policy.gate_with_review(channel, event, Some(reason)).await
        }
        roomkit_hooks::Decision::Continue => policy.gate(channel, event).await,
    }
}
