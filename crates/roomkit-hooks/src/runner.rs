use std::{path::Path, time::Duration};

use crate::CommandHook;

/// How long a handler's output is still read after it exits or is killed.
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
mod unix {
    use super::*;
    use rustix::process::{Pid, Signal, kill_process_group};
    use std::process::Stdio;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

    struct Group(Option<Pid>);
    impl Group {
        fn stop(&mut self) -> bool {
            match self.0.map(|pid| kill_process_group(pid, Signal::KILL)) {
                None | Some(Ok(())) | Some(Err(rustix::io::Errno::SRCH)) => {
                    self.0 = None;
                    true
                }
                _ => false,
            }
        }
    }
    impl Drop for Group {
        fn drop(&mut self) {
            self.stop();
        }
    }

    async fn capture(
        mut pipe: impl AsyncRead + Unpin,
        bytes: &mut Vec<u8>,
    ) -> Result<(), RunStatus> {
        let mut buffer = [0; 4096];
        loop {
            let n = pipe
                .read(&mut buffer)
                .await
                .map_err(|_| RunStatus::Failed)?;
            if n == 0 {
                return Ok(());
            }
            let remaining = 32_768 - bytes.len();
            bytes.extend_from_slice(&buffer[..n.min(remaining)]);
            if n > remaining {
                return Err(RunStatus::OutputLimit);
            }
        }
    }

    pub(super) async fn execute(
        hook: &CommandHook,
        cwd: &Path,
        input: &[u8],
    ) -> (RunStatus, Vec<u8>, Vec<u8>) {
        let mut child = match tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&hook.command)
            .current_dir(cwd)
            .env("ROOMKIT_PROJECT_DIR", cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(_) => return (RunStatus::Failed, vec![], vec![]),
        };
        let Some(pid) = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .filter(|id| *id > 1)
            .and_then(Pid::from_raw)
        else {
            return (RunStatus::Failed, vec![], vec![]);
        };
        let mut group = Group(Some(pid));
        let (Some(mut stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return (RunStatus::Failed, vec![], vec![]);
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let status = {
            let io = async {
                let write = async {
                    // A command may intentionally ignore stdin and exit immediately.
                    if let Err(error) = stdin.write_all(input).await
                        && error.kind() != std::io::ErrorKind::BrokenPipe
                    {
                        return Err(RunStatus::Failed);
                    }
                    drop(stdin);
                    Ok(())
                };
                tokio::try_join!(write, capture(stdout, &mut out), capture(stderr, &mut err))?;
                Ok::<_, RunStatus>(())
            };
            tokio::pin!(io);
            let mut io_done = false;
            let deadline = tokio::time::sleep(Duration::from_secs(hook.timeout));
            tokio::pin!(deadline);
            let mut status = loop {
                tokio::select! {
                    result = child.wait() => break result.ok().and_then(|s| s.code()).map_or(RunStatus::Failed, RunStatus::Exited),
                    result = &mut io, if !io_done => match result {
                        Ok(()) => io_done = true,
                        Err(status) => { io_done = true; break status; },
                    },
                    () = &mut deadline => break RunStatus::TimedOut,
                }
            };
            if !group.stop() {
                status = RunStatus::Failed;
            }
            // Kill before waiting/draining, including descendants holding the pipes.
            if child.wait().await.is_err() {
                status = RunStatus::Failed;
            }
            if !io_done {
                match tokio::time::timeout(Duration::from_millis(OUTPUT_DRAIN_MILLIS), &mut io)
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        if matches!(status, RunStatus::Exited(_)) {
                            status = error;
                        }
                    }
                    Err(_) => {
                        if matches!(status, RunStatus::Exited(_)) {
                            status = RunStatus::Failed;
                        }
                    }
                }
            }
            // Release capture borrows before returning the byte vectors.
            status
        };
        (status, out, err)
    }
}
