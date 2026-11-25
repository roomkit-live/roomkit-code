//! Resource-based placement for the experimental Redis executor pool.
mod cargo_test;
mod composer;
mod consultation;
pub(crate) use consultation::{ANSWER_PEER, ASK_PEER, PeerTool};
mod coordinator;
mod followup;
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
mod revision;
pub(crate) mod settings;
mod transport;
mod worker;
mod worker_options;

use registry::Worker;
pub(crate) use registry::WorkerProfile;

use std::{collections::BTreeMap, sync::Arc};
pub(crate) use worker::{advertise, worker_failure};
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

#[derive(Clone, Default, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Requirements {
    os: Option<String>,
    pub(crate) profile: Option<String>,
    labels: BTreeMap<String, String>,
    min_ram_mib: u64,
    consultation: bool,
    pub(crate) test_commit: Option<String>,
}
impl Requirements {
    pub(crate) fn for_remote_task(
        context: Option<&serde_json::Value>,
        pooled: bool,
    ) -> Result<Self, String> {
        if pooled {
            return Self::parse(context);
        }
        if context
            .and_then(|v| v.pointer("/executor/consultation"))
            .is_some_and(|v| v != &serde_json::Value::Bool(false))
        {
            return Err("Task consultation requires the executor pool".into());
        }
        if context
            .and_then(|v| v.pointer("/executor/test_commit"))
            .is_some()
        {
            return Err("A commit-test assignment requires the executor pool".into());
        }
        Ok(Self::default())
    }
    pub(crate) fn parse(context: Option<&serde_json::Value>) -> Result<Self, String> {
        let Some(value) = context.and_then(|v| v.get("executor")) else {
            return Ok(Self::default());
        };
        let req: Self = serde_json::from_value(value.clone())
            .map_err(|e| format!("Invalid executor requirements: {e}"))?;
        if req
            .test_commit
            .as_deref()
            .is_some_and(|sha| !revision::full_sha(sha))
        {
            return Err("test_commit requires a full Git commit SHA".into());
        }
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
    pub(crate) fn check_call(
        &self,
        name: &str,
        args: &roomkit::model::Metadata,
    ) -> Result<(), String> {
        if matches!(name, ASK_PEER | ANSWER_PEER) {
            return self
                .consultation
                .then_some(())
                .ok_or_else(|| "The parent must enable consultation for this task".into());
        }
        let Some(sha) = &self.test_commit else {
            return Ok(());
        };
        let asked = args.get("commit").and_then(|v| v.as_str());
        match name {
            CARGO_TEST if asked.is_some_and(|id| id.eq_ignore_ascii_case(sha)) => Ok(()),
            CARGO_TEST => Err(format!("This task requires cargo_test with commit {sha}")),
            LIST_EXECUTORS | "read_stored_result" => Ok(()),
            _ => Err("This task is limited to testing a Git commit. Its working files are private to cargo_test.".into())
        }
    }

    fn matches(&self, w: &Worker) -> bool {
        self.os.as_ref().is_none_or(|os| os == &w.os)
            && self.labels.iter().all(|(k, v)| w.labels.get(k) == Some(v))
            && self.accepts_profile(w)
    }

    fn accepts_profile(&self, w: &Worker) -> bool {
        let Some(profile) = w.profile(self.profile.as_deref()) else {
            return self.profile.is_none() && self.test_commit.is_none() && !self.consultation;
        };
        let offers = |name: &str| profile.tools.iter().any(|tool| tool == name);
        (self.test_commit.is_none() || (w.supports_commit_tests() && offers(CARGO_TEST)))
            && (!self.consultation || (offers(ASK_PEER) && offers(ANSWER_PEER)))
    }
}
pub(crate) struct Scheduler {
    group: String,
    scope: String,
    address: String,
    attending: tokio::sync::OnceCell<()>,
    turns: Arc<tokio::sync::RwLock<()>>,
    tasks: std::sync::Weak<crate::delegation::Tasks>,
    inventory: Inventory,
    queue: Arc<queue::Queue>,
    consultations: consultation::Consultations,
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
            scope: crate::digest::sha256_hex(serde_json::to_vec(&(group, &address))?),
            tasks: Arc::downgrade(tasks),
            inventory: Inventory::new(group, &address)?,
            address,
            queue: Arc::default(),
            consultations: Default::default(),
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
