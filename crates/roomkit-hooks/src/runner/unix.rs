use super::*;
use std::{future::Future, process::Stdio};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::ChildStdin,
    time::Instant,
};

mod group;
use group::Group;

fn failed(_stage: &str, _detail: impl std::fmt::Debug) -> RunStatus {
    // Keep syscall/stage diagnostics in native tests, never hook content.
    #[cfg(test)]
    eprintln!("Hook process failure at {_stage}: {_detail:?}");
    RunStatus::Failed
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exit_status(result: std::io::Result<std::process::ExitStatus>) -> RunStatus {
    match result {
        Ok(status) => status
            .code()
            .map_or_else(|| failed("child signal", status), RunStatus::Exited),
        Err(error) => failed("wait for child", error),
    }
}

fn spawn(hook: &CommandHook, cwd: &Path) -> Result<Group, RunStatus> {
    let child = tokio::process::Command::new("/bin/sh")
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
        .map_err(|error| failed("spawn child", error))?;
    Group::new(child)
}

async fn write_input(mut stdin: ChildStdin, input: &[u8]) -> Result<(), RunStatus> {
    // A command may intentionally ignore stdin and exit immediately.
    if let Err(error) = stdin.write_all(input).await
        && error.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(failed("write input pipe", error));
    }
    Ok(())
}

async fn capture(mut pipe: impl AsyncRead + Unpin, bytes: &mut Vec<u8>) -> Result<(), RunStatus> {
    let mut buffer = [0; 4096];
    loop {
        let n = pipe
            .read(&mut buffer)
            .await
            .map_err(|error| failed("read output pipe", error))?;
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

async fn supervise(
    group: &mut Group,
    io: impl Future<Output = Result<(), RunStatus>>,
    timeout: u64,
) -> RunStatus {
    tokio::pin!(io);
    let mut io_done = false;
    let deadline = tokio::time::sleep(Duration::from_secs(timeout));
    tokio::pin!(deadline);
    let mut status = loop {
        tokio::select! {
            result = group.wait() => break result,
            result = &mut io, if !io_done => match result {
                Ok(()) => io_done = true,
                Err(status) => { io_done = true; break status; },
            },
            () = &mut deadline => break RunStatus::TimedOut,
        }
    };
    if let Err(error) = group.stop() {
        status = error;
    }
    let cleanup_deadline = Instant::now() + Duration::from_millis(OUTPUT_DRAIN_MILLIS);
    if !io_done
        && let Err(error) = group.drain(&mut io, cleanup_deadline).await
        && matches!(status, RunStatus::Exited(_))
    {
        status = error;
    }
    if let Err(error) = group.reap(cleanup_deadline).await {
        status = error;
    }
    status
}

pub(super) async fn execute(
    hook: &CommandHook,
    cwd: &Path,
    input: &[u8],
) -> (RunStatus, Vec<u8>, Vec<u8>) {
    let mut group = match spawn(hook, cwd) {
        Ok(group) => group,
        Err(status) => return (status, vec![], vec![]),
    };
    let (Some(stdin), Some(stdout), Some(stderr)) = (
        group.child.stdin.take(),
        group.child.stdout.take(),
        group.child.stderr.take(),
    ) else {
        return (RunStatus::Failed, vec![], vec![]);
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let io = async {
        tokio::try_join!(
            write_input(stdin, input),
            capture(stdout, &mut out),
            capture(stderr, &mut err)
        )?;
        Ok(())
    };
    let status = supervise(&mut group, io, hook.timeout).await;
    (status, out, err)
}
