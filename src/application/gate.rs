//! The permission gate every tool call passes: Plan's restrictions first,
//! then the command hooks, then the application's policy and approval.

use std::sync::Arc;

use roomkit::{HookOptions, HookTrigger, RoomKit, ToolCallEvent};
use roomkit_sqlite::SqliteStore;
use roomkit_tui_engine::ConversationChannel;

use crate::permissions::{Policy, Verdict, block};
use crate::{Result, hooks::Hooks, plans::Plans};

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
    kit.hook(
        HookTrigger::BeforeToolUse,
        move |event: ToolCallEvent, _| {
            let gate = gate.clone();
            async move { decide(&gate, event).await }
        },
        HookOptions {
            name: "code-permissions".into(),
            timeout: crate::timeouts::PERMISSION_GATE,
            ..HookOptions::default()
        },
    )?;
    Ok(())
}

/// Plan refuses before any hook runs; a hook's refusal stands, and its
/// request for review reaches the approval card. A call outside a
/// conversation has no mode and no view to approve it.
async fn decide(gate: &Gate, event: ToolCallEvent) -> Verdict {
    let Gate {
        policy,
        channel,
        hooks,
        plans,
    } = gate;
    if event.room_id.is_none() {
        return block("This tool call has no conversation to approve it in.");
    }
    if policy.plan_blocks(&event) {
        return policy.gate(channel, event).await;
    }
    let review = match hooks.before_tool(&event).await {
        roomkit_hooks::Decision::Deny(reason) => return block(reason),
        roomkit_hooks::Decision::Ask(reason) => Some(reason),
        roomkit_hooks::Decision::Continue => None,
    };
    if event.name == crate::permissions::PLAN_EXIT {
        return crate::permissions::approve_exit(policy, channel, plans, event, review).await;
    }
    policy.gate_with_review(channel, event, review).await
}
