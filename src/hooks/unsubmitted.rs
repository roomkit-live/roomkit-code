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

use super::{CONFIGURATION_ERRORS, Context, Entry, Hooks};

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

/// Whether one of `entries` observes the outcome of a call of tool `name`.
fn observes_outcome(entries: &[Entry], name: &str) -> bool {
    entries.iter().any(|entry| {
        matches!(
            entry.event,
            HookEvent::PostToolUse | HookEvent::PostToolUseFailure
        ) && entry.group.matches(name)
    })
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

    /// Keeps a context for the outcome of the call `event` in `room`, when
    /// no view's turn holds the call and a hook observes that outcome: the
    /// engine reports it on a task of its own, which names no turn. Asked
    /// before anything can refuse the call, a refusal being an outcome too.
    /// No hook runs while the configuration does not read, an outcome's
    /// no more than any other.
    pub fn expect_outcome(&self, event: &ToolCallEvent, room: &Room) {
        if self.tool_turn(event).is_some() {
            return;
        }
        let observed = {
            let state = self.state();
            state.catalogue.problems.is_empty()
                && observes_outcome(&state.catalogue.selected(), &event.name)
        };
        let Some(call) = call(event).filter(|_| observed) else {
            return;
        };
        let turn = current_turn_id();
        let context = self.unsubmitted(room, &event.channel_id, turn.as_deref());
        let mut state = self.state();
        state
            .unsubmitted
            .retain(|_, kept| kept.at.elapsed() < KEPT_FOR);
        let at = std::time::Instant::now();
        state.unsubmitted.insert(call, Kept { at, context });
    }

    /// The PreToolUse hooks of a call no view's turn holds, in `room`. A
    /// configuration that does not read refuses it, as it refuses a view's
    /// turn; without a hook to run, the call makes no context.
    pub(super) async fn before_unsubmitted_tool(
        &self,
        event: &ToolCallEvent,
        room: &Room,
    ) -> Decision {
        let (unread, hooked) = {
            let state = self.state();
            let selected = state.catalogue.selected();
            let hooked = selected
                .iter()
                .any(|entry| entry.event == HookEvent::PreToolUse);
            (!state.catalogue.problems.is_empty(), hooked)
        };
        if unread {
            return Decision::Deny(CONFIGURATION_ERRORS.into());
        }
        if !hooked {
            return Decision::Continue;
        }
        let turn = current_turn_id();
        let context = self.unsubmitted(room, &event.channel_id, turn.as_deref());
        self.pre_tool(event, context).await
    }

    /// Runs the outcome hooks of a call [`Self::expect_outcome`] kept a
    /// context for; none for any other call.
    pub(super) fn after_unsubmitted_tool(
        self: &Arc<Self>,
        event: ToolCallEvent,
    ) -> Option<JoinHandle<()>> {
        let kept = self.state().unsubmitted.remove(&call(&event)?)?;
        let hooks = self.clone();
        let outcome = async move { hooks.post_tool(&event, kept.context).await };
        Some(self.background.spawn(outcome))
    }

    /// Forgets the context kept for the call `event`, whose outcome a
    /// view's turn answers: one that began between the call and its outcome.
    pub(super) fn answered_by_a_view(&self, event: &ToolCallEvent) {
        if let Some(call) = call(event) {
            self.state().unsubmitted.remove(&call);
        }
    }
}
