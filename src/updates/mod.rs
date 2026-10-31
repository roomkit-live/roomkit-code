//! Application distribution policy. No model, provider, or tool participates in updates.
mod install;
mod net;
#[cfg(test)]
mod tests;
mod ui;

use crate::Result;
use install::Installation;
use net::{GitHub, Release};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
pub const TARGET: &str = env!("ROOMKIT_BUILD_TARGET");
const MAX_BINARY: u64 = 256 * 1024 * 1024;
const INTERVAL: u64 = 24 * 60 * 60;
const NO_UPDATE: &str = "No new version available.";

fn version(raw: &str) -> Result<[u64; 3]> {
    let raw = raw.strip_prefix('v').unwrap_or(raw);
    let parts: Vec<_> = raw.split('.').collect();
    if raw.len() > 64
        || parts.len() != 3
        || parts.iter().any(|p| {
            p.is_empty()
                || (p.len() > 1 && p.starts_with('0'))
                || !p.bytes().all(|c| c.is_ascii_digit())
        })
    {
        return Err("Expected a stable release version such as 0.1.0".into());
    }
    Ok([parts[0].parse()?, parts[1].parse()?, parts[2].parse()?])
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    checked_at: u64,
    release: Option<Release>,
}
#[derive(Default)]
struct State {
    release: Option<Release>,
    notice: Option<String>,
    panel: Option<(String, roomkit_tui::protocol::PanelSpec)>,
    pending: Option<(String, String, Action)>,
}
#[derive(Clone)]
enum Action {
    Install(Release),
    Rollback(String),
}

pub struct Updates {
    diagnostics: Arc<crate::diagnostics::Diagnostics>,
    exe: PathBuf,
    installation: Option<Installation>,
    state: Mutex<State>,
    pub changes: tokio::sync::watch::Sender<()>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    stop: tokio_util::sync::CancellationToken,
    busy: Arc<tokio::sync::Mutex<()>>,
}
impl Updates {
    pub fn new(diagnostics: Arc<crate::diagnostics::Diagnostics>) -> Arc<Self> {
        let exe = std::env::current_exe().unwrap_or_default();
        let installation = Installation::detect(&exe);
        Arc::new(Self {
            diagnostics,
            exe,
            installation,
            state: Mutex::new(State::default()),
            changes: tokio::sync::watch::channel(()).0,
            task: Mutex::new(None),
            stop: tokio_util::sync::CancellationToken::new(),
            busy: Arc::new(tokio::sync::Mutex::new(())),
        })
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        crate::sync::lock(&self.state)
    }
    fn record_failure(&self, error: &(dyn std::error::Error + Send + Sync + 'static)) {
        let service = error.downcast_ref::<net::ServiceError>();
        self.diagnostics.record(
            "update_failed",
            serde_json::json!({
                "stage": match service {
                    Some(e) if e.asset => "download",
                    Some(_) => "check",
                    None => "operation",
                },
                "http_status": service.and_then(|e| e.status),
            }),
            Some(&error.to_string()),
        );
    }
    fn cache(&self) -> Option<PathBuf> {
        self.installation
            .as_ref()
            .map(|i| i.root().join("update-check.json"))
    }
    fn read_cache(&self) -> Cache {
        self.cache()
            .and_then(|p| {
                std::fs::metadata(&p)
                    .ok()
                    .filter(|m| m.len() <= 1024 * 1024)
                    .and_then(|_| std::fs::read(p).ok())
            })
            .and_then(|b| serde_json::from_slice::<Cache>(&b).ok())
            .unwrap_or_default()
    }
    fn save_cache(&self, release: Option<Release>) {
        let Some(path) = self.cache() else {
            return;
        };
        let save = || -> Result<()> {
            use std::io::Write;
            let mut temp =
                tempfile::NamedTempFile::new_in(path.parent().ok_or("Invalid update cache")?)?;
            temp.write_all(&serde_json::to_vec(&Cache {
                checked_at: now(),
                release,
            })?)?;
            temp.persist(path)?;
            Ok(())
        };
        let _ = save();
    }
    pub fn start(self: &Arc<Self>, enabled: bool) {
        if !enabled || self.installation.is_none() || env!("ROOMKIT_RELEASE_BUILD") != "1" {
            return;
        }
        let cache = self.read_cache();
        if let Some(release) = cache.release.filter(|r| r.validate().is_ok()) {
            self.state().release = Some(release);
        }
        self.changes.send_replace(());
        if cache.checked_at <= now() && now() - cache.checked_at < INTERVAL {
            return;
        }
        let this = self.clone();
        self.spawn(async move {
            let Ok(_guard) = this.busy.clone().try_lock_owned() else {
                return;
            };
            let check = async { GitHub::new().await?.latest().await };
            let result: Result<Option<Release>> =
                tokio::select! { r = check => r, () = this.stop.cancelled() => return };
            match result {
                Ok(release) => {
                    this.save_cache(release.clone());
                    this.state().release = release;
                }
                Err(error) => {
                    this.record_failure(error.as_ref());
                    this.save_cache(this.state().release.clone());
                }
            }
            this.changes.send_replace(());
        });
    }
    fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        *crate::sync::lock(&self.task) = Some(tokio::spawn(future));
    }
    pub async fn close(&self) {
        self.stop.cancel();
        let task = crate::sync::lock(&self.task).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
    pub fn label(&self) -> String {
        let s = self.state();
        if let Some(notice) = &s.notice {
            return format!("v{CURRENT} · {notice}");
        }
        if let Some(release) = &s.release
            && version(release.number()).ok() > version(CURRENT).ok()
        {
            return format!("v{CURRENT} · {} available · /update", release.tag_name);
        }
        format!("v{CURRENT} · /update")
    }
}

fn confirm(yes: bool) -> Result<bool> {
    use std::io::{IsTerminal, Write};
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err("Use --yes to confirm in non-interactive mode".into());
    }
    print!("Continue? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

pub async fn cli(args: &crate::config::Cli) -> Result<bool> {
    if let Some(bin) = &args.install_dir {
        if env!("ROOMKIT_RELEASE_BUILD") != "1" {
            return Err(
                "Managed installation requires a distribution build; use the release installer"
                    .into(),
            );
        }
        std::fs::create_dir_all(bin)?;
        let installation = Installation {
            bin: bin.canonicalize()?,
        };
        let exe = std::env::current_exe()?;
        let hash = install::digest(&exe)?;
        installation.activate(&exe, CURRENT, &hash, None)?;
        println!(
            "Installed RoomKit Code {CURRENT} in {}",
            installation.bin.display()
        );
        return Ok(true);
    }
    if !args.self_update && !args.rollback && !args.update_check {
        return Ok(false);
    }
    let exe = std::env::current_exe()?;
    let installation = Installation::detect(&exe).ok_or("This executable is not a managed installation. Use the release installer; Cargo and package-manager binaries are never replaced")?;
    installation.ensure_current(&exe)?;
    if args.rollback {
        let previous = installation.previous()?;
        println!(
            "Restore RoomKit Code {previous}? Current version: {CURRENT}. Application data is preserved."
        );
        if confirm(args.yes)? {
            installation.rollback(&exe, &previous)?;
            println!("Restored. Start RoomKit Code again to use the previous version.");
        }
    } else {
        let github = GitHub::new().await?;
        let Some(release) = github.latest().await? else {
            println!("{NO_UPDATE}");
            return Ok(true);
        };
        if version(release.number())? <= version(CURRENT)? {
            println!("{NO_UPDATE}");
            return Ok(true);
        }
        println!("Installed: {CURRENT}; latest stable: {}", release.number());
        if args.self_update && version(release.number())? > version(CURRENT)? && confirm(args.yes)?
        {
            let (file, checksum) = github.download(&release).await?;
            installation.activate(file.path(), release.number(), &checksum, Some(&exe))?;
            println!(
                "Updated. Start RoomKit Code again. Use --rollback to return to the previous binary."
            );
        }
    }
    Ok(true)
}
