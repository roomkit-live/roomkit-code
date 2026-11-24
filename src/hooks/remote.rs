//! Project command hooks for a delegated turn without an attached terminal.
use super::{Context, Hooks};
use roomkit::{ToolCallEvent, model::Room};
use roomkit_hooks::Decision;

pub(super) struct Pending {
    at: std::time::Instant,
    event: ToolCallEvent,
    context: Context,
}

impl Hooks {
    pub(crate) fn request_worker_report(
        &self,
        event: &ToolCallEvent,
        room: &Room,
        turn: &str,
    ) -> Option<String> {
        let context = self.worker_context(room, turn).ok()?;
        if !context.entries.iter().any(|entry| {
            matches!(
                entry.event,
                super::HookEvent::PostToolUse | super::HookEvent::PostToolUseFailure
            ) && entry.group.matches(&event.name)
        }) {
            return None;
        }
        let mut state = self.state();
        state
            .worker_reports
            .retain(|_, pending| pending.at.elapsed() < crate::remote::REPORT_LIFETIME);
        let key = uuid::Uuid::new_v4().to_string();
        state.worker_reports.insert(
            key.clone(),
            Pending {
                at: std::time::Instant::now(),
                event: event.clone(),
                context,
            },
        );
        Some(key)
    }

    fn worker_context(&self, room: &Room, turn: &str) -> Result<Context, String> {
        let parent = room
            .metadata
            .get("parent_room_id")
            .and_then(serde_json::Value::as_str)
            .ok_or("Missing worker parent conversation")?;
        if !self.state().catalogue.problems.is_empty() {
            return Err("Invalid hook configuration. Inspect /hooks on the coordinator.".into());
        }
        let agent =
            roomkit::model::ChannelId::new(crate::remote::AGENT).map_err(|e| e.to_string())?;
        let mut view = roomkit_tui_engine::SessionConfig::new(
            room.id.clone(),
            agent,
            crate::application::TERMINAL,
        );
        view.group = Some(parent.into());
        Ok(self.context(&self.session(&view), crate::remote::AGENT, Some(turn)))
    }

    pub(crate) async fn before_worker_tool(
        &self,
        event: &ToolCallEvent,
        room: &Room,
        turn: &str,
    ) -> Decision {
        match self.worker_context(room, turn) {
            Ok(context) => self.pre_tool(event, context).await,
            Err(error) => Decision::Deny(error),
        }
    }

    /// Between turns RoomKit authenticates the last executor, room and channel.
    /// The receipt binds the outcome to the original call, arguments and hook configuration.
    pub(crate) async fn finish_worker_report(
        &self,
        room: &Room,
        key: &str,
        report: crate::remote::ToolOutcome,
    ) -> Result<(), String> {
        let pending = {
            let mut state = self.state();
            if !state
                .worker_reports
                .get(key)
                .is_some_and(|p| p.event.room_id.as_ref() == Some(&room.id))
            {
                return Err("Unknown tool report for this room".into());
            }
            state
                .worker_reports
                .remove(key)
                .ok_or("Missing tool report")?
        };
        let mut event = pending.event;
        report.apply(&mut event);
        self.post_tool(&event, pending.context).await;
        Ok(())
    }
}
