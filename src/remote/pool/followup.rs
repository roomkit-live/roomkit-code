//! Address the active delegated turn using RoomKit steering.
use super::Scheduler;
use roomkit::model::{ChannelId, InjectMessage, Room, RoomId};
use roomkit::{ConversationStore, RoomKit, SteerOptions};
use roomkit_sqlite::SqliteStore;
use serde_json::{Value, json};

impl Scheduler {
    pub(crate) fn delegated_turn(
        &self,
        parent: &RoomId,
        child: &RoomId,
        task: &str,
    ) -> Option<(String, tokio_util::sync::CancellationToken)> {
        let (room, turn, closed) = self.queue.task_turn(parent.as_str(), task)?;
        (room == child.as_str()).then_some((turn, closed))
    }

    pub(crate) async fn send_to_task(
        &self,
        kit: &RoomKit<SqliteStore>,
        caller: &Room,
        task: &str,
        text: &str,
    ) -> Result<Value, String> {
        let (room, turn, _) = self.queue.task_turn(caller.id.as_str(), task)
            .ok_or("No running executor task with that ID is accepting messages in this conversation. Check task_status.")?;
        let directive = InjectMessage {
            role: "assistant".into(),
            content: format!(
                "Follow-up from the delegating conversation. Continue under the task's existing permissions. Return your result through the task.\n{}",
                json!({"message":text})
            ),
        };
        let options = SteerOptions {
            turn_id: Some(turn),
            organization_id: caller.organization_id.clone(),
        };
        let room = RoomId::new(room).map_err(|e| e.to_string())?;
        let agent = ChannelId::new(crate::remote::AGENT).map_err(|e| e.to_string())?;
        if kit
            .steer(&room, &agent, directive, options)
            .await
            .map_err(|e| e.to_string())?
            == 0
        {
            return Err("The worker has no active loop to receive this message".into());
        }
        Ok(
            json!({"status":"sent","to":format!("task:{task}"),"delivery":"The follow-up was accepted by the active task. Its final result returns to this conversation."}),
        )
    }
}

/// Preserve the original worker across owner restarts and pool changes.
pub(crate) async fn session_placement(
    kit: &RoomKit<SqliteStore>,
    child: &Room,
    parent: &Room,
) -> crate::Result<Option<Value>> {
    if let Some(target) = child
        .metadata
        .get("task_context")
        .and_then(|c| c.get("executor_target"))
    {
        return Ok(Some(target.clone()));
    }
    let Some(task) = child.metadata.get("task_id").and_then(Value::as_str) else {
        return Ok(None);
    };
    let id = roomkit::model::EventId::new(format!("task-placement-{task}"))?;
    let placed = kit
        .store()
        .get_event(&id)
        .await?
        .filter(|e| {
            e.room_id == parent.id && e.source.channel_id.as_str() == crate::application::TERMINAL
        })
        .and_then(|e| {
            e.metadata
                .get("task_placement")?
                .get("executor_target")
                .cloned()
        });
    Ok(placed)
}
