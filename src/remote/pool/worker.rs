//! Advertise the execution environment while this worker serves turns.
use super::{
    metrics::Sampler,
    registry::{Registry, Worker},
};
use crate::Result;
use std::{collections::BTreeMap, time::Duration};
use tokio::{task::JoinHandle, time::sleep};

#[derive(clap::Args, Debug, Default, Clone)]
pub(crate) struct WorkerArgs {
    /// Stable worker name (defaults to the Linux hostname).
    #[arg(long, requires="serve_pool",value_parser=super::identifier)]
    worker_name: Option<String>,
    /// Placement label, for example role=review. Repeat for more labels.
    #[arg(long, requires="serve_pool", value_parser=label)]
    worker_label: Vec<(String, String)>,
    /// Maximum concurrent remote turns (defaults to one).
    #[arg(long, requires="serve_pool",value_parser=clap::value_parser!(u16).range(1..=256))]
    worker_slots: Option<u16>,
    /// Physical host OS, when execution takes place inside a VM or container.
    #[arg(long, requires="serve_pool",value_parser=super::identifier)]
    worker_host_os: Option<String>,
}

fn label(raw: &str) -> std::result::Result<(String, String), String> {
    let (key, value) = raw
        .split_once('=')
        .ok_or("Use KEY=VALUE for a worker label")?;
    Ok((super::identifier(key)?, super::identifier(value)?))
}

impl WorkerArgs {
    pub(super) fn name(&self) -> Result<String> {
        if let Some(name) = &self.worker_name {
            return Ok(name.clone());
        }
        Ok(super::identifier(
            std::fs::read_to_string("/proc/sys/kernel/hostname")?.trim(),
        )?)
    }
    pub(crate) fn instance(&self) -> Result<String> {
        Ok(format!(
            "{}~{}",
            self.name()?,
            uuid::Uuid::new_v4().simple()
        ))
    }
}

pub(crate) struct Presence {
    heartbeat: JoinHandle<String>,
    registry: Registry,
    name: String,
    instance: String,
}
impl Presence {
    pub(crate) async fn retire(self) {
        self.heartbeat.abort();
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            self.registry.retire(&self.name, &self.instance),
        )
        .await;
    }
}
impl Drop for Presence {
    fn drop(&mut self) {
        self.heartbeat.abort();
    }
}
pub(crate) async fn worker_failure(presence: &mut Option<Presence>) -> String {
    match presence {
        Some(p) => (&mut p.heartbeat).await.unwrap_or_else(|e| e.to_string()),
        None => std::future::pending().await,
    }
}
pub(crate) fn declare(executor: &roomkit::TurnExecutor) -> Result<()> {
    executor.room_hook(
        &super::route(executor.executor_id()),
        roomkit::HookTrigger::OnToolCall,
        |_: roomkit::ToolCallEvent, _| async {},
        Default::default(),
    )?;
    Ok(())
}
pub(crate) async fn advertise(
    inventory: &super::Inventory,
    args: &WorkerArgs,
    model: String,
    executor: &roomkit::TurnExecutor,
) -> Result<Presence> {
    let registry = inventory.0.clone();
    let mut sampler = Sampler::default();
    let labels: BTreeMap<_, _> = args.worker_label.iter().cloned().collect();
    if labels.len() != args.worker_label.len() || labels.len() > 32 {
        return Err("Use up to 32 unique worker label keys".into());
    }
    let mut worker = Worker {
        name: args.name()?,
        instance: executor.executor_id().into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        model: Some(model),
        host_os: args.worker_host_os.clone(),
        labels,
        slots: args.worker_slots.unwrap_or(1),
        metrics: sampler.read(),
    };
    if !registry.publish(&worker).await? {
        return Err(format!("Worker name {} is already online", worker.name).into());
    }
    let name = worker.name.clone();
    let instance = worker.instance.clone();
    let registry_copy = registry.clone();
    let heartbeat = tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(2)).await;
            worker.metrics = sampler.read();
            if matches!(registry.publish(&worker).await, Ok(false)) {
                return format!("Worker name {} was taken by another instance", worker.name);
            }
        }
    });
    Ok(Presence {
        heartbeat,
        registry: registry_copy,
        name,
        instance,
    })
}
