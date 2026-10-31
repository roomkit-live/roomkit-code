//! Host deadlines. Each is derived from the limits it must outlast, so raising
//! a limit lengthens the deadlines that cover it instead of cutting the covered
//! work short.

use std::time::Duration;

/// Slack for the work around what a deadline covers: dispatch, encoding, I/O.
const MARGIN_SECONDS: u64 = 10;

/// The longest command a shell tool call may request; the system prompt
/// states it to the model.
pub(crate) const SHELL_MAX_SECONDS: u64 = 120;

/// One tool call. The longest is a shell command at its maximum.
pub(crate) const TOOL_CALL: Duration = Duration::from_secs(SHELL_MAX_SECONDS + MARGIN_SECONDS);

/// How long an approval card waits for the user's decision.
pub(crate) const APPROVAL: Duration = Duration::from_secs(APPROVAL_SECONDS);
const APPROVAL_SECONDS: u64 = 300;

/// Hook sources that run for one event: global, then project.
const HOOK_SOURCES: u64 = 2;

/// One event's command hooks, every source at its full budget.
pub(crate) const HOOKS: Duration = Duration::from_secs(HOOKS_SECONDS);
const HOOKS_SECONDS: u64 = HOOK_SOURCES * roomkit_hooks::SOURCE_BUDGET_SECONDS + MARGIN_SECONDS;

/// The permission gate runs the tool's hooks, then waits for an approval.
pub(crate) const PERMISSION_GATE: Duration =
    Duration::from_secs(HOOKS_SECONDS + APPROVAL_SECONDS + MARGIN_SECONDS);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_deadline_outlasts_what_it_covers() {
        assert!(TOOL_CALL > Duration::from_secs(SHELL_MAX_SECONDS));
        assert!(HOOKS > Duration::from_secs(2 * roomkit_hooks::SOURCE_BUDGET_SECONDS));
        assert!(PERMISSION_GATE > HOOKS + APPROVAL);
    }
}
