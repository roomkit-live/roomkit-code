//! Validated command hooks. Hosts own configuration discovery, trust, lifecycle,
//! permissions and presentation. Commands run with the host user's permissions.

mod config;
mod output;
mod runner;

pub use config::{CommandHook, Configuration, Event, HookGroup, Options};
pub use output::{Decision, interpret};
pub use runner::{Report, RunStatus, run};

#[cfg(all(test, unix))]
mod tests;
