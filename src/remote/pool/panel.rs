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
            let mut lines = scheduler
                .inventory()
                .lines()
                .await
                .unwrap_or_else(|reason| {
                    vec![format!(
                        "Inventory unavailable. {reason}. Check the connection and refresh."
                    )]
                });
            lines.extend(scheduler.queue.report(room));
            lines.join("\n")
        }
        None => String::from(
            "Scheduling is off for this session. Configure this workspace for the next launch.",
        ),
    };
    let panel = PanelSpec::new(
        uuid::Uuid::new_v4().to_string(),
        "executors",
        pool.map_or_else(
            || "Executor pool · beta".to_owned(),
            |p| format!("Executor pool · {} · beta", p.group),
        ),
        body,
    )
    .with_actions(vec![
        Choice::new("refresh", "Refresh"),
        Choice::new("configure", "Configure this workspace"),
    ])
    .with_dismiss("dismiss");
    Ok(CommandResult::Events(vec![Event::Panel {
        conversation: room.into(),
        panel,
    }]))
}
