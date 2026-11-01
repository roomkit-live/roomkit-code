//! The permission gate every tool call passes: Plan's restrictions first,
//! then the command hooks, then the application's policy and approval.

use std::sync::Arc;

use roomkit::{RoomKit, ToolCallEvent, ToolUse};
use roomkit_sqlite::SqliteStore;
use roomkit_tui_engine::ConversationChannel;

use crate::{Result, hooks::Hooks, permissions::Policy, plans::Plans};

/// What the gate decides with.
#[derive(Clone)]
pub(super) struct Gate {
    pub policy: Policy,
    pub channel: ConversationChannel,
    pub hooks: Arc<Hooks>,
    /// Leaving Plan shows the stored plan for approval.
    pub plans: Arc<Plans>,
}

pub(super) fn install(kit: &RoomKit<SqliteStore>, gate: Gate) -> Result<()> {
    kit.before_tool_use(
        roomkit::ToolHookSpec::new("code-permissions")
            .with_timeout(crate::timeouts::PERMISSION_GATE),
        move |event, _| {
            let gate = gate.clone();
            async move { Ok(decide(&gate, event).await) }
        },
    )?;
    Ok(())
}

/// Plan refuses before any hook runs; a hook's refusal stands, and its
/// request for review reaches the approval card.
async fn decide(gate: &Gate, event: ToolCallEvent) -> ToolUse {
    let Gate {
        policy,
        channel,
        hooks,
        plans,
    } = gate;
    if policy.plan_blocks(&event) {
        return policy.gate(channel, event).await;
    }
    let review = match hooks.before_tool(&event).await {
        roomkit_hooks::Decision::Deny(reason) => return ToolUse::block(reason),
        roomkit_hooks::Decision::Ask(reason) => Some(reason),
        roomkit_hooks::Decision::Continue => None,
    };
    if event.call.name == crate::permissions::PLAN_EXIT {
        return crate::permissions::approve_exit(policy, channel, plans, event, review).await;
    }
    policy.gate_with_review(channel, event, review).await
}
