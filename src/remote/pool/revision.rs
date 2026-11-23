//! Git facts about the worker checkout and disposable checkouts for approved test calls.
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
};

pub(super) fn full_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|c| c.is_ascii_hexdigit())
}

// Git operations use the given directory even when the executor was launched
// from a hook. Test code still receives the worker's normal command environment.
fn git(root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(root).env_clear();
    for name in ["PATH", "HOME", "TMPDIR", "SYSTEMROOT"] {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ALLOW_PROTOCOL", "file")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    cmd
}

async fn value(root: &Path, args: &[&str]) -> Option<String> {
    let out = git(root).args(args).output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    let mut bytes = out.stdout;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    String::from_utf8(bytes).ok()
}

async fn repo(root: &Path) -> Option<PathBuf> {
    Some(PathBuf::from(
        value(root, &["rev-parse", "--show-toplevel"]).await?,
    ))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Checkout {
    repository: PathBuf,
    branch: String,
    head: String,
    dirty: bool,
}

impl Checkout {
    pub(super) async fn sample(root: &Path) -> Option<Self> {
        tokio::time::timeout(Duration::from_secs(2), Self::status(root))
            .await
            .ok()?
    }

    async fn status(root: &Path) -> Option<Self> {
        let repository = repo(root).await?;
        // Read just the headers and first changed path, even in a large dirty tree.
        let mut child = git(&repository)
            .args([
                "status",
                "--porcelain=v2",
                "--branch",
                "--no-ahead-behind",
                "--untracked-files=normal",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let mut lines = BufReader::new(child.stdout.take()?.take(16384)).lines();
        let (mut head, mut branch, mut dirty) = (None, None, false);
        while let Some(line) = lines.next_line().await.ok()? {
            if let Some(id) = line.strip_prefix("# branch.oid ") {
                head = Some(id.to_owned());
            } else if let Some(name) = line.strip_prefix("# branch.head ") {
                branch = Some(name.to_owned());
            } else if !line.starts_with("# ") {
                dirty = true;
                break;
            }
        }
        if !dirty && !child.wait().await.ok()?.success() {
            return None;
        }
        Some(Self {
            repository,
            head: head?,
            branch: branch?,
            dirty,
        })
    }

    pub(super) fn display(&self) -> String {
        format!(
            "{} · {} · {} · {}",
            self.repository.display().to_string().escape_debug(),
            self.branch.escape_debug(),
            self.head.escape_debug(),
            if self.dirty { "dirty" } else { "clean" }
        )
    }
}

pub(super) struct TestCheckout {
    // The private clone borrows the local object database. Its refs, index and files
    // live in this directory, so cancellation requires no shared worktree bookkeeping.
    directory: Option<tempfile::TempDir>,
    pub root: PathBuf,
    pub head: String,
}

impl TestCheckout {
    pub(super) async fn open(workspace: &Path, sha: &str) -> Result<Self, String> {
        if !full_sha(sha) {
            return Err("Use the full Git commit SHA".into());
        }
        tokio::time::timeout(Duration::from_secs(60), Self::prepare(workspace, sha))
            .await
            .map_err(|_| "Preparing the test checkout timed out")?
    }

    // A Rust target directory can be large. Keep recursive removal off the
    // async executor, including when a running call is cancelled.
    pub(super) async fn close(mut self) -> Result<(), String> {
        if let Some(directory) = self.directory.take() {
            tokio::task::spawn_blocking(move || directory.close())
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("Could not remove the temporary test checkout: {e}"))?;
        }
        Ok(())
    }

    async fn prepare(workspace: &Path, sha: &str) -> Result<Self, String> {
        let repository = repo(workspace)
            .await
            .ok_or("The worker workspace needs a local Git checkout")?;
        if value(&repository, &["cat-file", "-t", sha])
            .await
            .as_deref()
            != Some("commit")
        {
            return Err(format!(
                "Commit {sha} is unavailable in the worker repository. Fetch it there and retry."
            ));
        }
        let workspace = tokio::fs::canonicalize(workspace)
            .await
            .map_err(|e| e.to_string())?;
        let prefix = workspace
            .strip_prefix(&repository)
            .map_err(|_| "Cannot locate the worker workspace within its Git repository")?;
        let (directory, tree) = Self::clone_at(&repository, sha).await?;
        let head = value(&tree, &["rev-parse", "HEAD"])
            .await
            .filter(|id| id.eq_ignore_ascii_case(sha))
            .ok_or("The private checkout HEAD differs from the requested commit")?;
        let tree = tokio::fs::canonicalize(tree)
            .await
            .map_err(|e| e.to_string())?;
        let root = tokio::fs::canonicalize(tree.join(prefix))
            .await
            .map_err(|_| "The worker workspace is missing at the requested commit")?;
        if !root.starts_with(&tree) {
            return Err("Workspace resolves outside the test checkout".into());
        }
        Ok(Self {
            directory: Some(directory),
            root,
            head,
        })
    }
    async fn clone_at(
        repository: &Path,
        sha: &str,
    ) -> Result<(tempfile::TempDir, PathBuf), String> {
        let directory = tempfile::Builder::new()
            .prefix("roomkit-cargo-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let tree = directory.path().join("tree");
        let cloned = git(repository)
            .args([
                "clone",
                "--local",
                "--shared",
                "--no-checkout",
                "--template=",
                "--",
            ])
            .arg(repository)
            .arg(&tree)
            .stdout(Stdio::null())
            .status()
            .await
            .map_err(|e| e.to_string())?;
        if !cloned.success() {
            return Err("Could not create the private local Git clone".into());
        }
        if value(&tree, &["checkout", "--detach", sha]).await.is_none() {
            return Err(format!("Could not check out commit {sha} on this worker"));
        }
        Ok((directory, tree))
    }
}

impl Drop for TestCheckout {
    fn drop(&mut self) {
        if let Some(directory) = self.directory.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn_blocking(move || drop(directory));
        }
    }
}

#[cfg(test)]
mod tests;
