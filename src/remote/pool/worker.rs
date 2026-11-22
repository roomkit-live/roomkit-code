//! Advertise the execution environment while this worker serves turns.
use super::{
    WorkerArgs,
    metrics::Sampler,
    registry::{Registry, Worker},
};
use crate::Result;
use std::time::Duration;
use tokio::{task::JoinHandle, time::sleep};

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
    profiles: Vec<super::WorkerProfile>,
    executor: &roomkit::TurnExecutor,
) -> Result<Presence> {
    let registry = inventory.0.clone();
    let mut sampler = Sampler::default();
    let mut worker = Worker {
        name: args.name()?,
        instance: executor.executor_id().into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        model: profiles.first().map(|p| p.model.clone()),
        profiles,
        host_os: args.host_os().map(str::to_owned),
        labels: args.labels(),
        slots: args.slots(),
        metrics: sampler.read().await,
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
            worker.metrics = sampler.read().await;
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
