//! Only explicitly supported model families select the separately installed OVH fork.
use super::{cache::Cache, hub::ModelFile};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

const REVISION: &str = "55c22dca0c987f247c21b427c3b3a09e1a28187f";

pub(super) fn resolve(model: &str, workspace: &Path) -> Result<String, String> {
    if model.starts_with("hf://") {
        let file = ModelFile::parse(model).ok_or("Invalid Hugging Face model selection")?;
        let cache = Cache::locate()?;
        if !cache.installed(&file) {
            return Err("The selected GGUF is missing. Download it again in /model.".into());
        }
        return Ok(cache.path(&file).to_string_lossy().into_owned());
    }
    if model.ends_with(".gguf") {
        let path = if let Some(rel) = model.strip_prefix("~/") {
            std::env::home_dir()
                .ok_or("Cannot resolve the model's home directory")?
                .join(rel)
        } else {
            workspace.join(model)
        };
        if !path.is_file() {
            return Err(format!("GGUF file does not exist: {}", path.display()));
        }
        return Ok(path.to_string_lossy().into_owned());
    }
    if let Some(path) = Cache::locate()?.reference(model) {
        return Ok(path.to_string_lossy().into_owned());
    }
    Ok(model.into())
}

fn needs_ovh(model: &str) -> bool {
    let repo = model
        .strip_prefix("hf://")
        .unwrap_or(model)
        .split(['@', ':'])
        .next()
        .unwrap_or("");
    if [
        "Tutanka01/CodeLALA-v1-7A1B-GGUF",
        "OVHaiLLM/Olala-v1.0-mini-7A1B-GGUF",
    ]
    .iter()
    .any(|r| repo.eq_ignore_ascii_case(r))
    {
        return true;
    }
    let name = model.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
    name.ends_with(".gguf")
        && (name.starts_with("codelala-v1-7a1b-") || name.starts_with("olala-v1.0-mini-7a1b-"))
}

pub(super) fn binary(model: &str) -> Result<(Option<PathBuf>, bool), String> {
    if let Some(value) = std::env::var_os("ROOMKIT_LLAMA_BINARY").filter(|v| !v.is_empty()) {
        let path = PathBuf::from(value);
        if !path.is_absolute() || !path.is_file() {
            return Err(
                "ROOMKIT_LLAMA_BINARY must be an absolute path to a llama-server executable".into(),
            );
        }
        return Ok((Some(path), false));
    }
    if !needs_ovh(model) {
        return Ok((None, false));
    }
    let root = crate::config::default_state_dir()
        .ok_or("Cannot locate per-user runtime storage")?
        .join("runtimes/ovh-llama.cpp")
        .join(REVISION);
    let path = root.join("bin/llama-server");
    Ok((Some(path), true))
}

pub(super) async fn ensure(path: &Path) -> Result<(), String> {
    if path.is_file() {
        return Ok(());
    }
    let root = path
        .parent()
        .and_then(Path::parent)
        .ok_or("Invalid runtime destination")?;
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("build.lock"))
        .map_err(|e| e.to_string())?;
    loop {
        match fs2::FileExt::try_lock_exclusive(&lock) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    if !path.is_file() {
        build(root, path, include_str!("install-ovh.sh")).await?;
    }
    Ok(())
}

// Git, CMake and their children inherit a private process group. Dropping the
// in-flight request stops the full build before releasing its lock and staging area.
struct BuildGroup(Option<rustix::process::Pid>);
impl Drop for BuildGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}

async fn build(root: &Path, path: &Path, script: &str) -> Result<(), String> {
    let stage = tempfile::Builder::new()
        .prefix(".build-")
        .tempdir_in(root)
        .map_err(|e| e.to_string())?;
    let log = tempfile::Builder::new()
        .prefix("build-")
        .suffix(".log")
        .tempfile_in(root)
        .map_err(|e| e.to_string())?;
    let output = log.reopen().map_err(|e| e.to_string())?;
    let mut child = tokio::process::Command::new("sh")
        .current_dir(stage.path())
        .args(["-c", script, "install-ovh"])
        .arg(path)
        .arg(REVISION)
        .stdin(Stdio::null())
        .stderr(output.try_clone().map_err(|e| e.to_string())?)
        .stdout(output)
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Could not start the OVH runtime build: {e}"))?;
    let mut group = BuildGroup(
        child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw),
    );
    let status = child.wait().await.map_err(|e| e.to_string())?;
    if !status.success() {
        let (_, log_path) = log.keep().map_err(|e| e.to_string())?;
        return Err(format!(
            "Could not build the OVH llama-server. See {}",
            log_path.display()
        ));
    }
    group.0 = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "fixture failures should stop the test")]
    use super::*;
    use tokio::time::{Duration, sleep, timeout};

    #[tokio::test]
    async fn cancelled_ovh_build_stops_children_and_removes_staging() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_owned();
        let done = root.join("installed");
        let task = tokio::spawn(async move {
            build(
                &root,
                &root.join("installed"),
                r#"
                ( : > "$1.started"; sleep 0.5; : > "$1" ) &
                wait
            "#,
            )
            .await
        });
        timeout(Duration::from_secs(5), async {
            while !done.with_extension("started").exists() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        sleep(Duration::from_millis(650)).await;
        assert!(!done.exists());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}
