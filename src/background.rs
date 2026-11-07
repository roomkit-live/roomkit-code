//! What runs in the background of a view, processes and delegated tasks
//! alike: what closing the application stops, and the footer's status.

use roomkit_tui::protocol::BackgroundStatus;

/// What closing stops, if anything: `processes` running processes and
/// `tasks` delegated tasks.
pub(crate) fn stopped(processes: usize, tasks: usize) -> Option<String> {
    let count = |n: usize, one: &str, many: &str| match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    };
    let parts: Vec<String> = [
        count(processes, "running process", "running processes"),
        count(tasks, "delegated task", "delegated tasks"),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| format!("{} will be stopped", parts.join(" and ")))
}

/// The view's background status: its running processes, which it opens, and
/// its conversation's running tasks, which it opens when no process runs.
pub(crate) fn status(processes: usize, tasks: usize) -> Option<BackgroundStatus> {
    let tasks_label = format!("Tasks: {tasks} running");
    let processes_label = format!("Processes: {processes} running");
    let (label, command) = match (processes, tasks) {
        (0, 0) => return None,
        (0, _) => (tasks_label, crate::delegation::panel::COMMAND),
        (_, 0) => (processes_label, "processes"),
        _ => (format!("{processes_label} · {tasks_label}"), "processes"),
    };
    Some(BackgroundStatus::new(label, command, ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closing names what it stops, processes and delegated tasks alike; the
    /// background status opens the processes while one runs, else the tasks.
    #[test]
    fn closing_and_the_background_status_count_processes_and_tasks() {
        assert_eq!(stopped(0, 0), None);
        assert_eq!(
            stopped(1, 0).as_deref(),
            Some("1 running process will be stopped")
        );
        assert_eq!(
            stopped(2, 1).as_deref(),
            Some("2 running processes and 1 delegated task will be stopped")
        );
        assert_eq!(
            stopped(0, 3).as_deref(),
            Some("3 delegated tasks will be stopped")
        );
        assert_eq!(status(0, 0), None);
        let shown = |p, t| status(p, t).map(|s| (s.label, s.command));
        assert_eq!(
            shown(0, 2),
            Some(("Tasks: 2 running".into(), "tasks".into()))
        );
        assert_eq!(
            shown(1, 0),
            Some(("Processes: 1 running".into(), "processes".into()))
        );
        assert_eq!(
            shown(1, 1),
            Some((
                "Processes: 1 running · Tasks: 1 running".into(),
                "processes".into()
            ))
        );
    }
}
