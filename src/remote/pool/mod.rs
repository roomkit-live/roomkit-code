//! Resource-based placement for the experimental Redis executor pool.
mod cargo_test;
pub(crate) use cargo_test::{CARGO_TEST, CargoTest};
mod inventory;
pub(crate) use inventory::{Inventory, LIST_EXECUTORS};
mod metrics;
mod panel;
pub(crate) use panel::panel;
mod queue;
mod registry;
mod transport;
mod worker;

pub(crate) use registry::WorkerProfile;
use registry::{Registry, Worker};
use roomkit::turns::TurnBackendHandle;
use std::{collections::BTreeMap, sync::Arc};
pub(crate) use worker::{WorkerArgs, advertise, declare, worker_failure};

fn identifier(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(
            "Use 1–64 letters, digits, dots, hyphens or underscores for names and labels".into(),
        );
    }
    Ok(value.into())
}
fn route(instance: &str) -> String {
    format!("pool-worker-{instance}")
}

#[derive(Clone, Default, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Requirements {
    os: Option<String>,
    pub(crate) profile: Option<String>,
    labels: BTreeMap<String, String>,
    min_ram_mib: u64,
}
impl Requirements {
    pub(crate) fn parse(context: Option<&serde_json::Value>) -> Result<Self, String> {
        let Some(value) = context.and_then(|v| v.get("executor")) else {
            return Ok(Self::default());
        };
        let req: Self = serde_json::from_value(value.clone())
            .map_err(|e| format!("Invalid executor requirements: {e}"))?;
        if let Some(profile) = &req.profile {
            crate::profiles::profile_id(profile)?;
        }
        if let Some(os) = &req.os {
            identifier(os)?;
        }
        if req.labels.len() > 32 {
            return Err("Use up to 32 executor labels".into());
        }
        for (k, v) in &req.labels {
            identifier(k)?;
            identifier(v)?;
        }
        Ok(req)
    }
    fn matches(&self, w: &Worker) -> bool {
        self.os.as_ref().is_none_or(|os| os == &w.os)
            && self
                .profile
                .as_ref()
                .is_none_or(|id| w.profiles.iter().any(|p| &p.id == id))
            && self.labels.iter().all(|(k, v)| w.labels.get(k) == Some(v))
    }
}
pub(crate) struct Scheduler {
    tasks: std::sync::Weak<crate::delegation::Tasks>,
    redis: roomkit::RedisTurnBackend,
    registry: Registry,
    queue: Arc<queue::Queue>,
    closed: tokio_util::sync::CancellationToken,
}
/// Owners place work and inspect the pool. Workers only need its inventory here.
pub(crate) fn connect(
    owner_group: Option<&str>,
    worker_group: Option<&str>,
    tasks: &Arc<crate::delegation::Tasks>,
) -> crate::Result<(Option<Arc<Scheduler>>, Option<Inventory>)> {
    let Some(group) = owner_group.or(worker_group) else {
        return Ok((None, None));
    };
    let inventory = Inventory::new(group)?;
    let scheduler = if owner_group.is_some() {
        Some(Arc::new(Scheduler {
            tasks: Arc::downgrade(tasks),
            redis: super::backend(&format!("pool:{group}"))?,
            registry: inventory.0.clone(),
            queue: Arc::default(),
            closed: Default::default(),
        }))
    } else {
        None
    };
    Ok((scheduler, Some(inventory)))
}

impl Scheduler {
    pub(crate) fn inventory(&self) -> Inventory {
        Inventory(self.registry.clone())
    }

    pub(crate) fn handle(self: &Arc<Self>) -> TurnBackendHandle {
        TurnBackendHandle::new(self.clone())
    }
}
