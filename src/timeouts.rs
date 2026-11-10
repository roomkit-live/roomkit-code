//! Host deadlines. Each is derived from the limits it must outlast, so raising
//! a limit lengthens the deadlines that cover it instead of cutting the covered
//! work short.

use std::time::Duration;

/// Slack for the work around what a deadline covers: dispatch, encoding, I/O,
/// killing a process.
const MARGIN_MILLIS: u64 = 10_000;

/// The longest command a shell tool call may request in the terminal.
const SHELL_MAX_SECONDS: u64 = 120;

/// The longest in an unattended launch: an hour, as for a process. Nobody
/// waits for its answer there, and a longer command would have to move to a
/// process the agent polls (HITL 884d7bf5).
const UNATTENDED_SHELL_MAX_SECONDS: u64 = 3600;

/// The longest command a shell tool call may request, in an unattended
/// launch or not; the system prompt states it to the model.
pub(crate) const fn shell_max_seconds(unattended: bool) -> u64 {
    if unattended {
        UNATTENDED_SHELL_MAX_SECONDS
    } else {
        SHELL_MAX_SECONDS
    }
}

/// One tool call, in an unattended launch or not. The longest is a shell
/// command at its maximum; the margin also covers the project snapshots the
/// file tracker takes around it, bounded at 50,000 files and 64 MiB read
/// (`files::snapshot`).
pub(crate) const fn tool_call(unattended: bool) -> Duration {
    Duration::from_millis(shell_max_seconds(unattended) * 1000 + MARGIN_MILLIS)
}

/// How long an approval card waits for the user's decision.
pub(crate) const APPROVAL: Duration = Duration::from_millis(APPROVAL_MILLIS);
const APPROVAL_MILLIS: u64 = 300_000;

/// Hook sources that run for one event: global, then project.
const HOOK_SOURCES: u64 = 2;

/// One event's command hooks, every source at its longest; closing waits
/// this long for a turn the engine is finishing, whose TurnEnd hooks run
/// to the end.
pub(crate) const HOOKS: Duration = Duration::from_millis(HOOKS_MILLIS);
const HOOKS_MILLIS: u64 = HOOK_SOURCES * roomkit_hooks::SOURCE_DEADLINE_MILLIS + MARGIN_MILLIS;

/// Closing waits this long for work that stops with the application: file
/// comparisons, and turns the engine accepted but never began to finish.
/// Only work that never ends waits it out.
pub(crate) const SETTLE: Duration = Duration::from_millis(MARGIN_MILLIS);

/// The permission gate runs the tool's hooks, then waits for an approval.
pub(crate) const PERMISSION_GATE: Duration =
    Duration::from_millis(HOOKS_MILLIS + APPROVAL_MILLIS + MARGIN_MILLIS);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_deadline_outlasts_what_it_covers() {
        for unattended in [false, true] {
            let shell = Duration::from_secs(shell_max_seconds(unattended));
            assert!(tool_call(unattended) > shell, "unattended: {unattended}");
        }
        assert!(shell_max_seconds(true) > shell_max_seconds(false));
        // A process read waits at most this long, within one tool call.
        let wait = Duration::from_millis(crate::agent::process_limits().max_wait_ms);
        assert!(tool_call(false) > wait && tool_call(true) > wait);
        // The most handlers a source allows, each at its timeout and drained.
        let handlers = roomkit_hooks::SOURCE_BUDGET_SECONDS / roomkit_hooks::MIN_HANDLER_SECONDS;
        let source = Duration::from_secs(roomkit_hooks::SOURCE_BUDGET_SECONDS)
            + Duration::from_millis(roomkit_hooks::OUTPUT_DRAIN_MILLIS) * handlers as u32;
        assert!(HOOKS > source * HOOK_SOURCES as u32);
        assert!(PERMISSION_GATE > HOOKS + APPROVAL);
        // A finishing turn's hooks get their budget, not the orphan wait.
        assert!(HOOKS > SETTLE);
    }
}
