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

/// A pool edit whose new connection answered, before the profiles are
/// locked to apply it.
pub(super) struct Connected<'a> {
    coordinator: &'a Coordinator,
    current: Option<Arc<Scheduler>>,
    next: Option<Arc<Scheduler>>,
}

impl<'a> Connected<'a> {
    /// Opens the pool `saved` asks for and checks that its Redis answers,
    /// when the edit changes the pool; none when it leaves it as it is. No
    /// lock is held here: Redis may take its whole deadline to answer.
    pub(super) async fn open(
        profiles: &'a Profiles,
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
        let next = group
            .map(|group| Scheduler::new(group, saved.url.as_deref(), &profiles.resources.tasks))
            .transpose()
            .map_err(|e| e.to_string())?;
        if let Some(pool) = &next
            && let Err(error) = pool.inventory().check().await
        {
            let _ = pool.close().await;
            return Err(error);
        }
        Ok(Some(Self {
            coordinator,
            current,
            next,
        }))
    }

    /// Closes the connection opened for an edit that is not applied.
    pub(super) async fn discard(self) {
        if let Some(pool) = self.next {
            let _ = pool.close().await;
        }
    }
}

impl<'a> Change<'a> {
    /// Stages `connected` on the profiles as `state` holds them: the agents
    /// assembled for the next pool, and the previous pool kept from new
    /// submissions.
    pub(super) async fn prepare(
        profiles: &'a Profiles,
        state: &State,
        connected: Connected<'a>,
    ) -> Result<Self, (String, Connected<'a>)> {
        match Self::stage(profiles, state, &connected).await {
            Ok((agents, permit)) => Ok(Self {
                coordinator: connected.coordinator,
                next: connected.next,
                agents,
                _permit: permit,
            }),
            Err(error) => Err((error, connected)),
        }
    }

    async fn stage(
        profiles: &Profiles,
        state: &State,
        connected: &Connected<'_>,
    ) -> Result<
        (
            Vec<(String, CodeAgent, Inventory)>,
            Option<OwnedRwLockWriteGuard<()>>,
        ),
        String,
    > {
        // The pool this edit was opened against is still the active one.
        let active = connected.coordinator.active();
        let unchanged = match (&active, &connected.current) {
            (None, None) => true,
            (Some(active), Some(current)) => Arc::ptr_eq(active, current),
            _ => false,
        };
        if !unchanged {
            return Err("The pool changed meanwhile; reopen /executors configure".into());
        }
        if profiles
            .resources
            .tasks
            .agent_running(crate::names::REMOTE_AGENT)
        {
            return Err(
                "Finish or cancel the queued or running remote tasks before changing pools".into(),
            );
        }
        let inventory = connected.next.as_ref().map(|pool| pool.inventory());
        let agents = prepare_agents(profiles, state, inventory).await?;
        let permit = connected
            .current
            .as_ref()
            .map(|pool| pool.edit_permit())
            .transpose()?;
        Ok((agents, permit))
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
        // The previous pool is closed whatever happens to the agents: an
        // agent that fails to install must not leave it open.
        let mut installed = Ok(());
        for (id, agent, inventory) in self.agents {
            let result = profiles.installed(kit, state, &id, agent, inventory).await;
            if installed.is_ok() {
                installed = result;
            }
        }
        if let Some(previous) = previous {
            previous
                .close()
                .await
                .map_err(|_| "Pool changed, but the previous connection did not close cleanly")?;
        }
        installed?;
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
