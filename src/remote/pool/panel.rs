//! Snapshot of registered workers and the current conversation's placements.
use super::Scheduler;
use roomkit_tui::protocol::{Choice, Event, PanelSpec};
use roomkit_tui_engine::{CommandResult, SessionConfig};

pub(crate) async fn panel(
    pool: Option<&Scheduler>,
    current: &SessionConfig,
    arguments: &str,
) -> Result<CommandResult, String> {
    if arguments == "dismiss" {
        return Ok(CommandResult::Events(vec![]));
    }
    let room = current.room.as_str();
    let body = match pool {
        Some(scheduler) => {
            let mut lines = scheduler.inventory().lines().await?;
            lines.extend(scheduler.queue.report(room));
            lines.join("\n")
        }
        None => "Start Code with --executor-pool GROUP to schedule tasks.".into(),
    };
    let panel = PanelSpec::new(
        uuid::Uuid::new_v4().to_string(),
        "executors",
        "Executor pool",
        body,
    )
    .with_actions(vec![Choice::new("refresh", "Refresh")])
    .with_dismiss("dismiss");
    Ok(CommandResult::Events(vec![Event::Panel {
        conversation: room.into(),
        panel,
    }]))
}
