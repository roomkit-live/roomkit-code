//! Per-workspace pool settings. CLI service invocations keep their explicit worker options.
use super::WorkerArgs;
use crate::{
    config::{Cli, Settings},
    storage::{document::Document, project_directory},
};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PoolSettings {
    pub group: Option<String>,
    pub schedule: bool,
    pub url: Option<String>,
    pub worker: WorkerArgs,
}

impl PoolSettings {
    pub fn open(state_dir: &Path, workspace: &Path) -> Result<Document<Self>, String> {
        Document::load(project_directory(state_dir, workspace).join("executors.json"))
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Some(group) = &self.group {
            crate::remote::group(group)?;
        } else if self.schedule {
            return Err("Enter a pool group to enable scheduling".into());
        }
        if let Some(url) = &self.url {
            crate::remote::validate_address(url)?;
        }
        self.worker.validate()
    }
}

pub(crate) fn apply(cli: &Cli, settings: &mut Settings) -> crate::Result<WorkerArgs> {
    if cli.local
        || cli.prompt.is_some()
        || cli.serve_worker.is_some()
        || cli.remote_worker.is_some()
    {
        return Ok(WorkerArgs::default());
    }
    let saved = PoolSettings::open(&settings.state_dir, &settings.workspace)?.value;
    if cli.executor {
        saved.validate()?;
        settings.serve_pool =
            Some(saved.group.ok_or(
                "Choose a group in /executors on this machine before starting --executor",
            )?);
        settings.executor_url = saved.url;
        return Ok(saved.worker);
    }
    if cli.serve_pool.is_some() {
        cli.worker.validate()?;
    } else if cli.executor_pool.is_none() && saved.schedule {
        let group = saved
            .group
            .as_deref()
            .ok_or("Missing saved executor group; open with --local to configure /executors")?;
        settings.executor_pool = Some(crate::remote::group(group)?);
    }
    settings.executor_url = saved.url;
    Ok(cli.worker.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::ffi::OsStr;

    #[test]
    fn launch_modes_choose_their_group_and_worker_options() -> crate::Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let launch = |extra: &[&str]| -> crate::Result<_> {
            let args = [
                OsStr::new("roomkit-code"),
                workspace.path().as_os_str(),
                OsStr::new("--state-dir"),
                state.path().as_os_str(),
            ];
            let cli = Cli::try_parse_from(args.into_iter().chain(extra.iter().map(OsStr::new)))?;
            let mut settings = Settings::load(&cli)?;
            let worker = apply(&cli, &mut settings)?;
            Ok((settings, worker))
        };
        let (first, _) = launch(&[])?;
        assert!(first.executor_pool.is_none());
        assert!(!state.path().join("projects").exists());
        crate::storage::Storage::open(&first.state_dir, &first.workspace)?;
        let mut document = PoolSettings::open(&first.state_dir, &first.workspace)?;
        document.save(PoolSettings {
            group: Some("saved".into()),
            schedule: true,
            worker: WorkerArgs::from_fields("laptop", "2", "role=review", "reviewer", "")?,
            ..Default::default()
        })?;
        assert_eq!(launch(&[])?.0.executor_pool.as_deref(), Some("saved"));
        assert!(launch(&["--local"])?.0.executor_pool.is_none());
        assert!(launch(&["--prompt", "hello"])?.0.executor_pool.is_none());
        assert!(
            launch(&["--remote-worker", "other"])?
                .0
                .executor_pool
                .is_none()
        );
        assert_eq!(
            launch(&["--executor-pool", "override"])?
                .0
                .executor_pool
                .as_deref(),
            Some("override")
        );
        let (service, worker) = launch(&["--executor"])?;
        assert_eq!(service.serve_pool.as_deref(), Some("saved"));
        assert!(service.executor_pool.is_none());
        assert_eq!(worker.worker_profile, ["reviewer"]);
        assert_eq!(worker.slots(), 2);
        let (explicit, worker) = launch(&["--serve-pool", "cli"])?;
        assert_eq!(explicit.serve_pool.as_deref(), Some("cli"));
        assert!(worker.worker_profile.is_empty());
        assert_eq!(worker.slots(), 1);
        Ok(())
    }
}
