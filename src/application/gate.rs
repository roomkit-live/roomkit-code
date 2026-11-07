//! The permission gate every tool call passes: Plan's restrictions first,
//! then the command hooks, then a delegation's target, then the application's
//! policy and approval.

use std::sync::Arc;

use roomkit::model::RoomContext;
use roomkit::{HookOptions, HookTrigger, RoomKit, ToolCallEvent, WeakRoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tui_engine::ConversationChannel;
use serde_json::Value;

use crate::delegation::{DELEGATE, SHARE_CHANNELS};
use crate::permissions::{Policy, Verdict, block};
use crate::{Result, hooks::Hooks, plans::Plans, profiles::Profiles};

/// What the gate decides with.
#[derive(Clone)]
pub(super) struct Gate {
    pub policy: Policy,
    pub channel: ConversationChannel,
    pub hooks: Arc<Hooks>,
    /// Leaving Plan shows the stored plan for approval.
    pub plans: Arc<Plans>,
    /// The agents a task can be handed to, and their assembly.
    pub profiles: Arc<Profiles>,
    /// The engine, held without keeping it alive: it holds this gate.
    pub kit: WeakRoomKit<SqliteStore>,
}

pub(super) fn install(kit: &RoomKit<SqliteStore>, gate: Gate) -> Result<()> {
    kit.hook(
        HookTrigger::BeforeToolUse,
        move |event: ToolCallEvent, context: RoomContext| {
            let gate = gate.clone();
            async move { decide(&gate, event, &context).await }
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
/// conversation has no mode and no view to approve it. A delegated task's
/// room takes its conversation's mode.
async fn decide(gate: &Gate, event: ToolCallEvent, context: &RoomContext) -> Verdict {
    let Gate {
        policy,
        channel,
        hooks,
        plans,
        ..
    } = gate;
    if event.room_id.is_none() {
        return block("This tool call has no conversation to approve it in.");
    }
    policy.adopt(&context.room);
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
    if event.name == DELEGATE
        && let Some(refusal) = delegation_refusal(gate, &event).await
    {
        return block(refusal);
    }
    policy.gate_with_review(channel, event, review).await
}

/// Why a `delegate_task` call cannot hand its task over, if it cannot: a
/// channel to share, an agent that is no other configured agent, or one that
/// cannot run, which is assembled here when it was never selected.
async fn delegation_refusal(gate: &Gate, event: &ToolCallEvent) -> Option<String> {
    if event
        .arguments
        .get(SHARE_CHANNELS)
        .is_some_and(|shared| !shared.is_null())
    {
        return Some(
            "share_channels is not available here: the agent works on its own. Put what it needs in the task or its context.".into(),
        );
    }
    let target = event
        .arguments
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let caller = event.channel_id.as_str();
    if let Some(refusal) = gate.profiles.resources.roster.refusal(caller, target) {
        return Some(refusal);
    }
    let kit = gate.kit.upgrade()?;
    let prepared = gate.profiles.prepare_target(&kit, target).await;
    prepared
        .err()
        .map(|error| format!("Agent {target} cannot work on a task: {error}"))
}
