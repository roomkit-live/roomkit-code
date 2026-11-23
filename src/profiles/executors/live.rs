//! Stage the connection and agent tools before committing an interactive pool edit.
use super::{PoolSettings, Profiles, State};
use crate::{
    agent::{CodeAgent, Inventory},
    remote::pool::{self, Coordinator, Scheduler},
};
use roomkit::RoomKit;
use roomkit_sqlite::SqliteStore;
use std::sync::Arc;
use tokio::sync::OwnedRwLockWriteGuard;

pub(super) struct Change<'a> {
    coordinator: &'a Coordinator,
    next: Option<Arc<Scheduler>>,
    agents: Vec<(String, CodeAgent, Inventory)>,
    // Keep new submissions out of the retired queue until it is closed.
    _permit: Option<OwnedRwLockWriteGuard<()>>,
}

impl<'a> Change<'a> {
    pub(super) async fn prepare(
        profiles: &'a Profiles,
        state: &State,
        saved: &PoolSettings,
    ) -> Result<Option<Self>, String> {
        let Some(coordinator) = &profiles.pool else {
            return Ok(None);
        };
        let current = coordinator.active();
        let group = saved.group.as_deref().filter(|_| saved.schedule);
        match (&current, group) {
            (None, None) => return Ok(None),
            (Some(pool), Some(group))
                if pool
                    .matches_connection(group, saved.url.as_deref())
                    .map_err(|e| e.to_string())? =>
            {
                return Ok(None);
            }
            _ => {}
        }
        if profiles.resources.tasks.agent_running(crate::remote::AGENT) {
            return Err(
                "Finish or cancel the queued or running remote tasks before changing pools".into(),
            );
        }
        let next = group
            .map(|group| Scheduler::new(group, saved.url.as_deref(), &profiles.resources.tasks))
            .transpose()
            .map_err(|e| e.to_string())?;
        let inventory = next.as_ref().map(|pool| pool.inventory());
        if let Some(inventory) = &inventory {
            inventory.check().await?;
        }
        let agents = prepare_agents(profiles, state, inventory).await?;
        let permit = current
            .as_ref()
            .map(|pool| pool.edit_permit())
            .transpose()?;
        Ok(Some(Self {
            coordinator,
            next,
            agents,
            _permit: permit,
        }))
    }

    async fn apply(
        self,
        profiles: &Profiles,
        kit: &RoomKit<SqliteStore>,
        state: &mut State,
    ) -> Result<&'static str, String> {
        let enabled = self.next.is_some();
        let previous = self.coordinator.replace(self.next);
        state.remote_worker = enabled;
        state.roster.enable_pool(enabled);
        state.refresh_roster();
        for (id, agent, inventory) in self.agents {
            profiles
                .installed(kit, state, &id, agent, inventory)
                .await?;
        }
        if let Some(previous) = previous {
            previous
                .close()
                .await
                .map_err(|_| "Pool changed, but the previous connection did not close cleanly")?;
        }
        Ok(if enabled {
            "Executor settings saved. Scheduling is on."
        } else {
            "Executor settings saved. Scheduling is off."
        })
    }
}

async fn prepare_agents(
    profiles: &Profiles,
    state: &State,
    inventory: Option<pool::Inventory>,
) -> Result<Vec<(String, CodeAgent, Inventory)>, String> {
    let mut agents = Vec::new();
    for id in state.tools.keys() {
        let Some(profile) = state.agent(id) else {
            continue;
        };
        let provider = state
            .providers
            .value
            .profiles
            .get(&profile.provider)
            .ok_or("Unknown agent connection")?;
        let (agent, tools) = profiles
            .assemble_with_inventory(&profile, provider, None, id, inventory.clone())
            .await?;
        agents.push((id.clone(), agent, tools));
    }
    Ok(agents)
}

pub(super) async fn save(
    profiles: &Profiles,
    kit: &RoomKit<SqliteStore>,
    state: &mut State,
    saved: PoolSettings,
    change: Option<Change<'_>>,
) -> Result<&'static str, String> {
    let document = &mut state
        .executor_form
        .as_mut()
        .ok_or("Reopen /executors configure")?
        .document;
    let toggled = change
        .as_ref()
        .map(|c| c.next.is_some())
        .filter(|&next| next != state.remote_worker);
    if let Some(enabled) = toggled {
        set_declaration(kit, enabled)?;
    }
    if let Err(error) = document.save(saved) {
        if let Some(enabled) = toggled {
            set_declaration(kit, !enabled)?;
        }
        return Err(error);
    }
    state.executor_form = None;
    match change {
        Some(change) => change.apply(profiles, kit, state).await,
        None if profiles.pool.is_none() => {
            Ok("Executor settings saved. This launch keeps its explicit mode.")
        }
        None => Ok("Executor settings saved."),
    }
}

fn set_declaration(kit: &RoomKit<SqliteStore>, enabled: bool) -> Result<(), String> {
    if enabled {
        State::declare_remote(kit)
    } else {
        State::undeclare_remote(kit)
    }
}
