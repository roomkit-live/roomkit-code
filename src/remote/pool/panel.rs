//! Snapshot of registered workers and the current conversation's placements.
use super::{Scheduler, registry::Row};
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
                .registry
                .rows()
                .await?
                .iter()
                .map(describe)
                .collect::<Vec<_>>();
            if lines.is_empty() {
                lines.push("No workers registered.".into());
            }
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
fn describe(row: &Row) -> String {
    let worker = &row.worker;
    let (name, os, arch) = (&worker.name, &worker.os, &worker.arch);
    let host = worker.host_os.as_deref().unwrap_or("unspecified");
    let online = if row.online() { "online" } else { "offline" };
    let free = if row.online() {
        worker.slots.saturating_sub(row.used)
    } else {
        0
    };
    let metrics = &worker.metrics;
    let busy = metrics
        .busy
        .map_or_else(|| "sampling".into(), |n| format!("{n}% busy"));
    let load = metrics
        .load
        .map_or_else(|| "unknown".into(), |n| format!("{n:.2}"));
    let labels = worker
        .labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{name} [{online}] {os}/{arch} · host {host} · {} CPU ({busy}) · load {load} · {} MiB available · {free}/{} slots free · {labels}",
        metrics.cpus, metrics.available_mib, worker.slots
    )
}
