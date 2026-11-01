//! Host deadlines. Each is derived from the limits it must outlast, so raising
//! a limit lengthens the deadlines that cover it instead of cutting the covered
//! work short.

use std::time::Duration;

/// Slack for the work around what a deadline covers: dispatch, encoding, I/O,
/// killing a process.
const MARGIN_MILLIS: u64 = 10_000;

/// The longest command a shell tool call may request; the system prompt
/// states it to the model.
pub(crate) const SHELL_MAX_SECONDS: u64 = 120;

/// One tool call. The longest is a shell command at its maximum; the margin
/// also covers the project snapshots the file tracker takes around it,
/// bounded at 50,000 files and 64 MiB read (`files::snapshot`).
pub(crate) const TOOL_CALL: Duration =
    Duration::from_millis(SHELL_MAX_SECONDS * 1000 + MARGIN_MILLIS);

/// How long an approval card waits for the user's decision.
pub(crate) const APPROVAL: Duration = Duration::from_millis(APPROVAL_MILLIS);
const APPROVAL_MILLIS: u64 = 300_000;

/// Hook sources that run for one event: global, then project.
const HOOK_SOURCES: u64 = 2;

/// One event's command hooks, every source at its longest.
pub(crate) const HOOKS: Duration = Duration::from_millis(HOOKS_MILLIS);
const HOOKS_MILLIS: u64 = HOOK_SOURCES * roomkit_hooks::SOURCE_DEADLINE_MILLIS + MARGIN_MILLIS;

/// Closing waits this long for work that stops with the application: hook
/// turns and tasks, file comparisons. Only work that never ends waits it out.
pub(crate) const SETTLE: Duration = Duration::from_millis(MARGIN_MILLIS);

/// The permission gate runs the tool's hooks, then waits for an approval.
pub(crate) const PERMISSION_GATE: Duration =
    Duration::from_millis(HOOKS_MILLIS + APPROVAL_MILLIS + MARGIN_MILLIS);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_deadline_outlasts_what_it_covers() {
        assert!(TOOL_CALL > Duration::from_secs(SHELL_MAX_SECONDS));
        // The most handlers a source allows, each at its timeout and drained.
        let handlers = roomkit_hooks::SOURCE_BUDGET_SECONDS / roomkit_hooks::MIN_HANDLER_SECONDS;
        let source = Duration::from_secs(roomkit_hooks::SOURCE_BUDGET_SECONDS)
            + Duration::from_millis(roomkit_hooks::OUTPUT_DRAIN_MILLIS) * handlers as u32;
        assert!(HOOKS > source * HOOK_SOURCES as u32);
        assert!(PERMISSION_GATE > HOOKS + APPROVAL);
    }
}
