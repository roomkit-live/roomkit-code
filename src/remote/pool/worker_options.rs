//! Local worker identity, capacity and explicitly exposed profiles.
use super::identifier;
use std::collections::{BTreeMap, BTreeSet};

#[derive(clap::Args, Debug, Default, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct WorkerArgs {
    /// Local agent profile to serve. Repeat to expose more; the first is the default.
    #[arg(long, requires = "serve_pool", value_parser = crate::profiles::profile_id)]
    pub(crate) worker_profile: Vec<String>,
    /// Stable worker name (defaults to the hostname).
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
    pub(super) fn name(&self) -> crate::Result<String> {
        if let Some(name) = &self.worker_name {
            return Ok(name.clone());
        }
        Ok(super::identifier(
            rustix::system::uname().nodename().to_str()?,
        )?)
    }
    pub(crate) fn instance(&self) -> crate::Result<String> {
        Ok(format!(
            "{}~{}",
            self.name()?,
            uuid::Uuid::new_v4().simple()
        ))
    }

    pub fn validate(&self) -> Result<(), String> {
        for value in self.worker_name.iter().chain(&self.worker_host_os) {
            identifier(value)?;
        }
        if self.worker_slots.is_some_and(|n| !(1..=256).contains(&n)) {
            return Err("Use 1–256 worker slots".into());
        }
        let mut keys = BTreeSet::new();
        for (key, value) in &self.worker_label {
            identifier(key)?;
            identifier(value)?;
            if !keys.insert(key) || keys.len() > 32 {
                return Err("Use up to 32 unique worker label keys".into());
            }
        }
        let mut profiles = BTreeSet::new();
        for id in &self.worker_profile {
            crate::profiles::profile_id(id)?;
            if !profiles.insert(id) {
                return Err(format!("Duplicate worker profile {id}"));
            }
        }
        Ok(())
    }

    pub fn label_text(&self) -> String {
        self.worker_label
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn from_fields(
        name: &str,
        slots: &str,
        labels: &str,
        profiles: &str,
        host_os: &str,
    ) -> Result<Self, String> {
        fn items(text: &str) -> impl Iterator<Item = &str> {
            text.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
        }
        let args = Self {
            worker_name: (!name.is_empty()).then(|| name.to_owned()),
            worker_slots: (!slots.is_empty())
                .then(|| slots.parse().map_err(|_| "Use 1–256 worker slots"))
                .transpose()?,
            worker_label: items(labels).map(label).collect::<Result<_, _>>()?,
            worker_profile: items(profiles).map(str::to_owned).collect(),
            worker_host_os: (!host_os.is_empty()).then(|| host_os.to_owned()),
        };
        args.validate()?;
        Ok(args)
    }
    pub fn configured_name(&self) -> &str {
        self.worker_name.as_deref().unwrap_or("")
    }
    pub fn slots(&self) -> u16 {
        self.worker_slots.unwrap_or(1)
    }
    pub fn labels(&self) -> BTreeMap<String, String> {
        self.worker_label.iter().cloned().collect()
    }

    pub fn host_os(&self) -> Option<&str> {
        self.worker_host_os.as_deref()
    }
}
