use std::{future::Future, time::Duration};

use rustix::process::{Pid, Signal, kill_process_group};
use tokio::{process::Child, time::Instant};

use super::{RunStatus, failed};

type Kill = fn(Pid, Signal) -> rustix::io::Result<()>;

/// Own the leader until all destructive signals have finished.
pub(super) struct Group {
    pub(super) child: Child,
    pid: Option<Pid>,
    kill: Kill,
    permission_denied: bool,
}

impl Group {
    pub(super) fn new(child: Child) -> Result<Self, RunStatus> {
        let pid = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .filter(|id| *id > 1)
            .and_then(Pid::from_raw)
            .ok_or(RunStatus::Failed)?;
        Ok(Self {
            child,
            pid: Some(pid),
            kill: kill_process_group,
            permission_denied: false,
        })
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn observed_exit(&self) -> Result<Option<RunStatus>, RunStatus> {
        use rustix::process::{WaitId, WaitIdOptions, waitid};
        let pid = self.pid.ok_or(RunStatus::Failed)?;
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        let status = match waitid(WaitId::Pid(pid), options) {
            Ok(status) => status,
            Err(rustix::io::Errno::INTR) => return Ok(None),
            Err(error) => return Err(failed("observe child without reaping", error)),
        };
        Ok(status.map(|status| {
            status
                .exit_status()
                .map_or_else(|| failed("child signal", status), RunStatus::Exited)
        }))
    }

    pub(super) async fn wait(&mut self) -> RunStatus {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        loop {
            match self.observed_exit() {
                Ok(Some(status)) | Err(status) => return status,
                Ok(None) => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
        // Preserve the existing single-signal behavior on other Unix systems.
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        super::exit_status(self.child.wait().await)
    }

    pub(super) fn stop(&mut self) -> Result<(), RunStatus> {
        let Some(pid) = self.pid else { return Ok(()) };
        match (self.kill)(pid, Signal::KILL) {
            Ok(()) => {
                // Darwin snapshots group members before signalling them. A
                // concurrent fork can miss this pass, so retain the identity.
                #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                {
                    self.pid = None;
                }
                Ok(())
            }
            Err(rustix::io::Errno::SRCH) => {
                self.pid = None;
                Ok(())
            }
            #[cfg(target_os = "macos")]
            Err(rustix::io::Errno::PERM) if self.observed_exit()?.is_some() => {
                // XNU also reports EPERM for a group containing only zombies.
                // Keep this provisional until the group disappears after reap.
                self.permission_denied = true;
                Ok(())
            }
            Err(error) => Err(failed("stop process group", error)),
        }
    }

    pub(super) async fn drain(
        &mut self,
        io: impl Future<Output = Result<(), RunStatus>>,
        deadline: Instant,
    ) -> Result<(), RunStatus> {
        tokio::pin!(io);
        tokio::time::timeout_at(deadline, async {
            loop {
                tokio::select! {
                    biased;
                    result = &mut io => return result,
                    () = tokio::time::sleep(Duration::from_millis(10)) => self.stop()?,
                }
            }
        })
        .await
        .map_err(|error| failed("drain output after cleanup", error))?
    }

    pub(super) async fn reap(&mut self, deadline: Instant) -> Result<(), RunStatus> {
        // No signal may use this PGID after wait releases the leader's PID.
        let pid = self.pid.take();
        tokio::time::timeout_at(deadline, self.child.wait())
            .await
            .map_err(|error| failed("deadline reaping child after cleanup", error))?
            .map_err(|error| failed("reap child after cleanup", error))?;
        if self.permission_denied
            && let Some(pid) = pid
        {
            confirm_group_gone(pid, deadline).await?;
        }
        Ok(())
    }
}

async fn confirm_group_gone(pid: Pid, deadline: Instant) -> Result<(), RunStatus> {
    // Signal zero cannot kill a recycled PID. Only ESRCH resolves the deferred
    // permission error; a live, inaccessible or recycled group remains failure.
    loop {
        if rustix::process::test_kill_process_group(pid) == Err(rustix::io::Errno::SRCH) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(failed("confirm process group disappeared", pid));
        }
        tokio::time::sleep_until((Instant::now() + Duration::from_millis(5)).min(deadline)).await;
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        let _stopped = self.stop();
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
