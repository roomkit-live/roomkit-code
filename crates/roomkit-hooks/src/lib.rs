//! Validated command hooks. Hosts own configuration discovery, trust, lifecycle,
//! permissions and presentation. Commands run with the host user's permissions.

mod config;
mod output;
mod runner;

pub use config::{
    CommandHook, Configuration, Event, HookGroup, MAX_HANDLER_SECONDS, MIN_HANDLER_SECONDS,
    Options, SOURCE_BUDGET_SECONDS,
};
pub use output::{Decision, interpret};
pub use runner::{OUTPUT_DRAIN_MILLIS, Report, RunStatus, run};

/// The longest one event's handlers from one source can take, in
/// milliseconds: the whole budget spent by the most handlers it allows, each
/// followed by its output drain. Killing a handler takes a little longer.
pub const SOURCE_DEADLINE_MILLIS: u64 = SOURCE_BUDGET_SECONDS * 1000
    + SOURCE_BUDGET_SECONDS / MIN_HANDLER_SECONDS * OUTPUT_DRAIN_MILLIS;

#[cfg(all(test, unix))]
mod tests;
