use std::{path::Path, time::Duration};

use crate::CommandHook;

/// Cleanup budget for remaining output and reaping after a handler exits or is killed.
pub const OUTPUT_DRAIN_MILLIS: u64 = 250;

/// Bounded subprocess outcome, separate from a policy decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunStatus {
    /// Normal process exit, including nonzero codes.
    Exited(i32),
    /// The time bound expired; remaining process-group members were killed.
    TimedOut,
    /// Captured output exceeded its byte bound.
    OutputLimit,
    /// The host cancelled the execution; the dropped future cleans up its group.
    Cancelled,
    /// Command could not start, its pipes failed, or process cleanup failed.
    Failed,
}

impl RunStatus {
    /// Safe summary containing no command or output content.
    pub fn label(&self) -> String {
        match self {
            Self::Exited(code) => format!("exit {code}"),
            Self::TimedOut => "timed out".into(),
            Self::OutputLimit => "output limit exceeded".into(),
            Self::Cancelled => "cancelled".into(),
            Self::Failed => "process error".into(),
        }
    }
}

/// Captured output is for explicit inspection, not automatic diagnostic logging.
#[derive(Clone, Debug)]
pub struct Report {
    /// Process outcome.
    pub status: RunStatus,
    /// At most 32 KiB, decoded lossily. Oversized output is never parsed as a decision.
    pub stdout: String,
    /// At most 32 KiB, decoded lossily.
    pub stderr: String,
    /// Wall-clock duration including cleanup.
    pub elapsed: Duration,
}

/// Execute a validated hook with one JSON object and newline on stdin.
/// No user text is interpolated into the command. Both streams are drained
/// concurrently. Unix process groups are killed on timeout, normal exit and
/// future cancellation; this is cleanup, not a sandbox against escaping children.
pub async fn run(hook: &CommandHook, cwd: &Path, input: &serde_json::Value) -> Report {
    let started = std::time::Instant::now();
    let mut bytes = serde_json::to_vec(input).unwrap_or_default();
    bytes.push(b'\n');
    if bytes.len() > 1_048_576
        || !input.is_object()
        || !(1..=30).contains(&hook.timeout)
        || hook.kind != "command"
        || hook.command.trim().is_empty()
        || hook.command.len() > 8192
        || hook.command.contains('\0')
    {
        return Report {
            status: RunStatus::Failed,
            stdout: String::new(),
            stderr: String::new(),
            elapsed: started.elapsed(),
        };
    }
    #[cfg(unix)]
    let result = unix::execute(hook, cwd, &bytes).await;
    #[cfg(not(unix))]
    let result = (RunStatus::Failed, Vec::new(), Vec::new());
    Report {
        status: result.0,
        stdout: decoded(&result.1),
        stderr: decoded(&result.2),
        elapsed: started.elapsed(),
    }
}

fn decoded(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    text.truncate(text.floor_char_boundary(32_768.min(text.len())));
    text
}

#[cfg(unix)]
mod unix;
