//! Address the active delegated turn using RoomKit steering.
use super::Scheduler;
use roomkit::model::{ChannelId, InjectMessage, Room, RoomId};
use roomkit::{RoomKit, SteerOptions};
use roomkit_sqlite::SqliteStore;
use serde_json::{Value, json};

impl Scheduler {
    pub(crate) async fn send_to_task(
        &self,
        kit: &RoomKit<SqliteStore>,
        caller: &Room,
        task: &str,
        text: &str,
    ) -> Result<Value, String> {
        let (room, turn) = self.queue.task_turn(caller.id.as_str(), task)
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
