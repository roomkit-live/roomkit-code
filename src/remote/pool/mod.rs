//! Resource-based placement for the experimental Redis executor pool.
mod metrics;
mod panel;
pub(crate) use panel::panel;
mod queue;
mod registry;
mod transport;
mod worker;

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
            && self.labels.iter().all(|(k, v)| w.labels.get(k) == Some(v))
    }
}
pub(crate) struct Scheduler {
    redis: roomkit::RedisTurnBackend,
    registry: Registry,
    queue: Arc<queue::Queue>,
    closed: tokio_util::sync::CancellationToken,
}
impl Scheduler {
    pub(crate) fn new(group: &str) -> crate::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            redis: super::backend(&format!("pool:{group}"))?,
            registry: Registry::new(group)?,
            queue: Arc::default(),
            closed: Default::default(),
        }))
    }
    pub(crate) fn handle(self: &Arc<Self>) -> TurnBackendHandle {
        TurnBackendHandle::new(self.clone())
    }
}
