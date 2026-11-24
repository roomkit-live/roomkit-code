//! Resource-based placement for the experimental Redis executor pool.
mod cargo_test;
mod coordinator;
pub(crate) use cargo_test::{CARGO_TEST, CargoTest};
pub(crate) use coordinator::Coordinator;
mod inventory;
pub(crate) use inventory::{Inventory, LIST_EXECUTORS};
mod lease;
mod metrics;
mod panel;
pub(crate) use panel::panel;
mod queue;
mod registry;
pub(crate) mod settings;
mod transport;
mod worker;
mod worker_options;

use registry::Worker;
pub(crate) use registry::WorkerProfile;

use std::{collections::BTreeMap, sync::Arc};
pub(crate) use worker::{advertise, declare, worker_failure};
pub(crate) use worker_options::WorkerArgs;

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
    group: String,
    address: String,
    attending: tokio::sync::OnceCell<()>,
    turns: Arc<tokio::sync::RwLock<()>>,
    tasks: std::sync::Weak<crate::delegation::Tasks>,
    inventory: Inventory,
    queue: Arc<queue::Queue>,
    closed: tokio_util::sync::CancellationToken,
}
impl Scheduler {
    pub(crate) fn new(
        group: &str,
        saved_url: Option<&str>,
        tasks: &Arc<crate::delegation::Tasks>,
    ) -> crate::Result<Arc<Self>> {
        let address = super::address(saved_url)?;
        Ok(Arc::new(Self {
            group: group.into(),
            tasks: Arc::downgrade(tasks),
            inventory: Inventory::new(group, &address)?,
            address,
            queue: Arc::default(),
            closed: Default::default(),
            attending: Default::default(),
            turns: Default::default(),
        }))
    }

    pub(crate) fn matches_connection(
        &self,
        group: &str,
        saved_url: Option<&str>,
    ) -> crate::Result<bool> {
        Ok(self.group == group && self.address == super::address(saved_url)?)
    }

    pub(crate) fn edit_permit(&self) -> Result<tokio::sync::OwnedRwLockWriteGuard<()>, String> {
        self.turns.clone().try_write_owned().map_err(|_| {
            "Finish or cancel the queued or running remote tasks before changing pools".into()
        })
    }

    pub(crate) fn group(&self) -> &str {
        &self.group
    }

    pub(crate) fn inventory(&self) -> Inventory {
        self.inventory.clone()
    }
}
