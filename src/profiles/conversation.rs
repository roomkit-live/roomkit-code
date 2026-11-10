//! The model and the effort an agent runs with in a conversation, which the
//! conversation keeps (`sessions::settings`): the agent's own when it starts
//! working there, then what `/model` and `/effort` set there. The agent's
//! own, set in `/agents`, are what a new conversation starts with: changing
//! them leaves the conversations that exist as they are.
//!
//! An agent holds one configuration of its own, and one more for each
//! conversation this instance opened that keeps another model or effort for
//! it: its turns there run with that one (`slot`).

use roomkit_tui_engine::CommandResult;

use super::*;
use crate::sessions::Running;

/// The model and the effort that are `agent`'s own.
pub(super) fn own(agent: &Agent) -> Running {
    Running {
        model: agent.model.clone(),
        effort: agent.reasoning_effort.clone(),
    }
}

impl State {
    /// Agent `id` as it runs in `room`: its profile, with the model and the
    /// effort the conversation keeps for it.
    pub(super) fn running_agent(&self, room: &str, id: &str) -> Option<Agent> {
        let mut agent = self.agent(id)?;
        if let Some(running) = self.running.get(&(room.to_owned(), id.to_owned())) {
            agent.model.clone_from(&running.model);
            agent.reasoning_effort.clone_from(&running.effort);
        }
        Some(agent)
    }
}

impl Profiles {
    /// Makes `view` show its agent as it runs in its conversation, with the
    /// model and the effort the conversation keeps for it.
    pub async fn settle(&self, kit: &RoomKit<SqliteStore>, view: &mut SessionConfig) {
        let mut state = self.state.lock().await;
        self.settle_in(kit, &mut state, view).await;
    }

    pub(super) async fn settle_in(
        &self,
        kit: &RoomKit<SqliteStore>,
        state: &mut State,
        view: &mut SessionConfig,
    ) {
        self.recall(kit, state, view).await;
        self.fit(state, view.room.as_str(), view.agent.as_str())
            .await;
        state.view(view);
    }

    /// Reads what `view`'s conversation keeps for its agent, the first time
    /// this instance shows the agent there. A conversation that keeps
    /// nothing for it yet does from then on. A task's room keeps nothing:
    /// its agent runs there with its own.
    async fn recall(&self, kit: &RoomKit<SqliteStore>, state: &mut State, view: &SessionConfig) {
        let (room, id) = (view.room.as_str(), view.agent.as_str());
        let key = (room.to_owned(), id.to_owned());
        if self.resources.tasks.root_of(room) != room || state.running.contains_key(&key) {
            return;
        }
        let Some(profile) = state.agent(id) else {
            return;
        };
        let kept = crate::sessions::kept(kit, &view.room).await;
        let running = match kept.agents.get(id) {
            Some(running) => running.clone(),
            None => {
                let running = self.first(kit, state, view, &profile).await;
                let first = running.clone();
                // Kept in memory if not in the store: this launch honors it.
                let _ = crate::sessions::keep(kit, &view.room, |settings| {
                    settings.agents.insert(id.to_owned(), first);
                })
                .await;
                running
            }
        };
        state.running.insert(key, running);
    }

    /// What `profile`'s agent runs with in `view`'s conversation, which
    /// keeps nothing for it: its own model and effort. In a conversation it
    /// already answered in, stored before conversations kept their
    /// settings, the model is that of its last answer there, when it came
    /// from the kind of provider the agent still uses.
    async fn first(
        &self,
        kit: &RoomKit<SqliteStore>,
        state: &State,
        view: &SessionConfig,
        profile: &Agent,
    ) -> Running {
        let mut running = own(profile);
        let connection = state.providers.value.profiles.get(&profile.provider);
        let kind = connection.map(|connection| connection.kind.name());
        let answered = crate::sessions::last_model(kit, &view.room, &view.agent).await;
        if let Some((provider, model)) = answered
            && Some(provider.as_str()) == kind
        {
            running.model = model;
        }
        running
    }

    /// Gives agent `id` the configuration it runs with in `room`: its own
    /// when the conversation keeps the agent's own model and effort, else
    /// one assembled for the conversation. One that cannot be assembled
    /// leaves the agent's turns there refused, with the reason, rather than
    /// run on another model.
    pub(super) async fn fit(&self, state: &mut State, room: &str, id: &str) {
        let Some(slot) = state.slots.get(id).cloned() else {
            return;
        };
        let key = (room.to_owned(), id.to_owned());
        let compactions = &self.resources.compactions;
        // What an earlier configuration registered for the conversation.
        compactions.forget_in(room, id);
        state.unfitted.remove(&key);
        let kept = state
            .agent(id)
            .zip(state.running.get(&key).cloned())
            .filter(|(profile, running)| own(profile) != *running);
        let Some((mut profile, running)) = kept else {
            slot.unfit(room);
            return;
        };
        profile.model.clone_from(&running.model);
        profile.reasoning_effort = running.effort;
        let assembled = match state.providers.value.profiles.get(&profile.provider) {
            Some(provider) => self.assemble_for(&profile, provider, Some(room)).await,
            None => Err("Configure the agent's provider with /agents".into()),
        };
        match assembled {
            Ok((runtime, _)) => slot.fit(room, Ok(runtime)),
            Err(error) => {
                compactions.forget_in(room, id);
                let reason = format!(
                    "This conversation's model, {}, cannot be used: {error}. Choose one with /model",
                    running.model
                );
                slot.fit(room, Err(reason.clone()));
                state.unfitted.insert(key, reason);
            }
        }
    }

    /// Installs `runtime` as agent `id`'s own configuration, then gives the
    /// conversations this instance opened the one each keeps for it,
    /// assembled anew: what the agent's profile holds beside its model and
    /// effort, its instructions and tools, changed with it.
    pub(super) async fn installed(
        &self,
        kit: &RoomKit<SqliteStore>,
        state: &mut State,
        id: &str,
        runtime: CodeAgent,
        inventory: Inventory,
    ) -> Result<(), String> {
        state.install(kit, id, runtime, inventory)?;
        let rooms: Vec<String> = state
            .running
            .keys()
            .filter(|(_, agent)| agent == id)
            .map(|(room, _)| room.clone())
            .collect();
        for room in rooms {
            self.fit(state, &room, id).await;
        }
        Ok(())
    }

    /// Makes `running` what `view`'s agent runs with in its conversation,
    /// which keeps it. A task's room keeps nothing.
    pub(super) async fn adopt(
        &self,
        kit: &RoomKit<SqliteStore>,
        state: &mut State,
        view: &SessionConfig,
        running: Running,
    ) -> Result<(), String> {
        let (room, id) = (view.room.as_str(), view.agent.as_str());
        if self.resources.tasks.root_of(room) != room {
            return Ok(());
        }
        let kept = running.clone();
        crate::sessions::keep(kit, &view.room, |settings| {
            settings.agents.insert(id.to_owned(), kept);
        })
        .await
        .map_err(|_| "Could not keep this conversation's model and effort")?;
        state
            .running
            .insert((room.to_owned(), id.to_owned()), running);
        self.fit(state, room, id).await;
        Ok(())
    }

    /// `/model` and `/effort` with a value: changes what the current agent
    /// runs with in this conversation, which keeps it. Nothing else changes:
    /// not the agent's own model and effort, set in `/agents`, nor what it
    /// runs with in another conversation.
    pub(super) async fn change_current(
        &self,
        kit: &RoomKit<SqliteStore>,
        state: &mut State,
        current: SessionConfig,
        change: impl FnOnce(&mut Agent),
    ) -> Result<CommandResult, String> {
        let (room, id) = (current.room.as_str(), current.agent.as_str());
        if self.resources.tasks.root_of(room) != room {
            return Err(
                "A task runs with its agent's own model and effort; change them in its conversation"
                    .into(),
            );
        }
        let mut agent = state
            .running_agent(room, id)
            .ok_or("Configure an agent with /agents first")?;
        change(&mut agent);
        agent.validate()?;
        let (key, running) = ((room.to_owned(), id.to_owned()), own(&agent));
        let before = state.running.insert(key.clone(), running.clone());
        self.fit(state, room, id).await;
        // A model or an effort the agent cannot run with, or that cannot be
        // kept, changes nothing.
        let refused = match state.unfitted.get(&key) {
            Some(reason) => Some(reason.clone()),
            None => crate::sessions::keep(kit, &current.room, |settings| {
                settings.agents.insert(id.to_owned(), running);
            })
            .await
            .err()
            .map(|_| "Could not keep this conversation's model and effort".to_owned()),
        };
        if let Some(reason) = refused {
            match before {
                Some(before) => state.running.insert(key, before),
                None => state.running.remove(&key),
            };
            self.fit(state, room, id).await;
            return Err(reason);
        }
        let mut next = current;
        state.view(&mut next);
        Ok(CommandResult::Switch(Box::new(next)))
    }
}
