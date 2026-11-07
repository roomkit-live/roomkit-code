//! The agents a delegated task can be handed to (RMK-1532): the roster every
//! turn reads, kept as the agent files change, and a target's agent assembled
//! from its current settings as a task is handed to it.

use super::*;

impl State {
    /// Sets the roster to the agents this project can use, as the agent files
    /// read last list them; none while no agent is served `delegate_task`.
    pub(super) fn refresh_roster(&self) {
        let agents = match self.delegation {
            Some(_) => self
                .agents()
                .into_iter()
                .map(|(agent, _)| (agent.id, agent.label))
                .collect(),
            None => Vec::new(),
        };
        self.roster.set(agents);
    }
}

impl Profiles {
    /// Makes agent `id` ready for a task handed to it: its slot holds its
    /// agent, assembled now from its profile, its connection, its skills and
    /// its MCP servers as they stand. A connection, skill or server edit
    /// reinstalls only the selected agent, so an agent assembled earlier may
    /// hold a key or an endpoint since replaced. A turn of that agent already
    /// running keeps the agent it began with. The view's selection is
    /// unchanged.
    ///
    /// # Errors
    ///
    /// Why the agent cannot run: unknown, or its connection or credential
    /// refused.
    pub(crate) async fn prepare_target(
        &self,
        kit: &RoomKit<SqliteStore>,
        id: &str,
    ) -> Result<(), String> {
        let mut state = self.state.lock().await;
        let agent = state.agent(id).ok_or("Unknown agent")?;
        let provider = state
            .providers
            .value
            .profiles
            .get(&agent.provider)
            .ok_or("The agent references an unknown connection")?;
        let (runtime, inventory) = self.assemble(&agent, provider).await?;
        state.put(kit, id, runtime, inventory)
    }
}
