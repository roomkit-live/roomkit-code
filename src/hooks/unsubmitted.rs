//! Command hooks for the calls of a turn no view submitted: one the engine
//! opened by itself, as when a background process ends, an agent is handed
//! a task or a task's result comes back. No view opened it, so the
//! lifecycle holds no turn for it: each call makes its own context, from
//! its room and the engine's turn, and keeps it for its outcome.
use std::sync::Arc;

use roomkit::model::{ChannelId, Room};
use roomkit::{ToolCallEvent, current_turn_id};
use roomkit_hooks::{Decision, Event as HookEvent};
use roomkit_tui_engine::SessionConfig;
use tokio::task::JoinHandle;

use super::{Context, Hooks};

/// What a turn is refused, and a call outside one, while the hook
/// configuration does not read.
pub(super) const CONFIGURATION_ERRORS: &str =
    "Hook configuration has errors; inspect /hooks and reload after correcting it";

/// How long a call's context waits for its outcome: a call may wait for the
/// person's approval as long as a worker's report waits for its own.
const KEPT_FOR: std::time::Duration = crate::remote::REPORT_LIFETIME;

/// The context a call made, kept for its outcome's hooks.
pub(super) struct Kept {
    at: std::time::Instant,
    context: Context,
}

/// A call, by its room and its ID.
pub(super) type Call = (String, String);

fn call(event: &ToolCallEvent) -> Option<Call> {
    let room = event.room_id.as_ref()?;
    Some((room.to_string(), event.tool_call_id.clone()))
}

impl Hooks {
    /// The context of `agent`'s calls in `room` under the engine's turn
    /// `turn`. A delegated task's room belongs to its conversation's
    /// session, as a view moved into it does.
    pub(super) fn unsubmitted(
        &self,
        room: &Room,
        agent: &ChannelId,
        turn: Option<&str>,
    ) -> Context {
        let mut view = SessionConfig::new(room.id.clone(), agent.clone(), crate::names::TERMINAL);
        view.group = room
            .metadata
            .get("parent_room_id")
            .and_then(serde_json::Value::as_str)
            .map(Into::into);
        self.context(&self.session(&view), agent.as_str(), turn)
    }

    /// The PreToolUse hooks of a call no view's turn holds, in `room`.
    pub(super) async fn before_unsubmitted_tool(
        &self,
        event: &ToolCallEvent,
        room: &Room,
    ) -> Decision {
        if !self.state().catalogue.problems.is_empty() {
            return Decision::Deny(CONFIGURATION_ERRORS.into());
        }
        let turn = current_turn_id();
        let context = self.unsubmitted(room, &event.channel_id, turn.as_deref());
        self.keep_for_outcome(event, &context);
        self.pre_tool(event, context).await
    }

    /// Keeps `context` for the outcome of the call `event`, when a hook
    /// observes that outcome: the engine reports it on a task of its own,
    /// which names no turn.
    fn keep_for_outcome(&self, event: &ToolCallEvent, context: &Context) {
        let observed = context.entries.iter().any(|entry| {
            matches!(
                entry.event,
                HookEvent::PostToolUse | HookEvent::PostToolUseFailure
            ) && entry.group.matches(&event.name)
        });
        let Some(call) = call(event).filter(|_| observed) else {
            return;
        };
        let mut state = self.state();
        state
            .unsubmitted
            .retain(|_, kept| kept.at.elapsed() < KEPT_FOR);
        let kept = Kept {
            at: std::time::Instant::now(),
            context: context.clone(),
        };
        state.unsubmitted.insert(call, kept);
    }

    /// Runs the outcome hooks of a call [`Self::before_unsubmitted_tool`]
    /// kept a context for; none for any other call.
    pub(super) fn after_unsubmitted_tool(
        self: &Arc<Self>,
        event: ToolCallEvent,
    ) -> Option<JoinHandle<()>> {
        let kept = self.state().unsubmitted.remove(&call(&event)?)?;
        let hooks = self.clone();
        let outcome = async move { hooks.post_tool(&event, kept.context).await };
        Some(self.background.spawn(outcome))
    }
}
