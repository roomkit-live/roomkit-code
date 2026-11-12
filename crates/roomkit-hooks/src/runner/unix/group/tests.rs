#![allow(
    clippy::unwrap_used,
    reason = "Test setup and process assertions fail the test directly"
)]

use super::*;
use std::{
    process::Stdio,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

fn spawn(command: &str) -> Group {
    let child = tokio::process::Command::new("/bin/sh")
        .args(["-c", command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    Group::new(child).unwrap()
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_millis(crate::OUTPUT_DRAIN_MILLIS)
}

fn assert_stopped(pid: i32) {
    let state = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .unwrap();
    assert!(
        state.status.success() || state.status.code() == Some(1),
        "{state:?}"
    );
    let text = String::from_utf8_lossy(&state.stdout);
    assert!(
        text.trim().is_empty() || text.trim().starts_with(['Z', 'X']),
        "{state:?}"
    );
}

#[tokio::test]
async fn a_member_missed_by_the_first_signal_is_stopped_before_reaping_the_leader() {
    let mut group = spawn("sleep 30 & echo $!; exit 0");
    let mut output = BufReader::new(group.child.stdout.take().unwrap());
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), output.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let descendant: i32 = line.trim().parse().unwrap();
    assert_eq!(group.wait().await, RunStatus::Exited(0));
    // Model Darwin's partial delivery: the first call misses the descendant.
    group.kill = rustix::process::kill_process;
    group.stop().unwrap();
    group.kill = kill_process_group;
    let deadline = deadline();
    group
        .drain(
            async {
                output
                    .read_to_end(&mut Vec::new())
                    .await
                    .map_err(|_| RunStatus::Failed)?;
                Ok(())
            },
            deadline,
        )
        .await
        .unwrap();
    assert_eq!(group.observed_exit().unwrap(), Some(RunStatus::Exited(0)));
    group.reap(deadline).await.unwrap();
    assert_stopped(descendant);
}

#[tokio::test]
async fn normal_exit_keeps_its_pid_until_reap_and_never_signals_afterward() {
    static SIGNALS: AtomicUsize = AtomicUsize::new(0);
    fn count(pid: Pid, signal: Signal) -> rustix::io::Result<()> {
        SIGNALS.fetch_add(1, Ordering::SeqCst);
        kill_process_group(pid, signal)
    }
    let mut group = spawn("exit 7");
    group.kill = count;
    assert_eq!(group.wait().await, RunStatus::Exited(7));
    assert_eq!(group.observed_exit().unwrap(), Some(RunStatus::Exited(7)));
    group.stop().unwrap();
    group.reap(deadline()).await.unwrap();
    let before = SIGNALS.load(Ordering::SeqCst);
    group.stop().unwrap();
    drop(group);
    assert_eq!(SIGNALS.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn a_permission_error_for_a_live_leader_is_not_ignored() {
    let mut group = spawn("sleep 30");
    group.kill = |_, _| Err(rustix::io::Errno::PERM);
    assert_eq!(group.stop(), Err(RunStatus::Failed));
    group.kill = kill_process_group;
    group.stop().unwrap();
    group.reap(deadline()).await.unwrap();
}

#[tokio::test]
async fn a_leader_that_refuses_cleanup_does_not_hold_reaping_open() {
    let mut group = spawn("exec sleep 30");
    group.kill = |_, _| Err(rustix::io::Errno::PERM);
    assert_eq!(group.stop(), Err(RunStatus::Failed));
    let result = tokio::time::timeout(Duration::from_secs(1), group.reap(deadline())).await;
    // The injected refusal leaves a real child alive; clean it even on failure.
    group.child.kill().await.unwrap();
    assert_eq!(result, Ok(Err(RunStatus::Failed)));
}

#[tokio::test]
async fn a_group_that_still_exists_does_not_resolve_a_deferred_permission_error() {
    let mut group = spawn("sleep 30");
    let pid = group.pid.unwrap();
    assert_eq!(
        confirm_group_gone(pid, Instant::now()).await,
        Err(RunStatus::Failed)
    );
    group.stop().unwrap();
    group.reap(deadline()).await.unwrap();
}

#[tokio::test]
async fn an_output_pipe_kept_outside_the_group_still_fails_at_the_original_deadline() {
    let mut group = spawn("exit 0");
    assert_eq!(group.wait().await, RunStatus::Exited(0));
    group.stop().unwrap();
    let deadline = deadline();
    assert_eq!(
        group.drain(std::future::pending(), deadline).await,
        Err(RunStatus::Failed)
    );
    group.reap(deadline).await.unwrap();
}
