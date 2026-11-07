//! The agents a delegated task can be handed to (RMK-1532): the roster every
//! turn reads, kept as the agent files change, and a target's agent assembled
//! as a task is handed to it, when it was never selected.

use super::*;

impl State {
    /// Sets the roster to the agents this project can use, as the agent files
    /// read last list them.
    pub(super) fn refresh_roster(&self) {
        let agents = self
            .agents()
            .into_iter()
            .map(|(agent, _)| (agent.id, agent.label))
            .collect();
        self.roster.set(agents);
    }
}

impl Profiles {
    /// Makes agent `id` ready for a task handed to it: its slot holds its
    /// agent, assembled now if it was never selected. The view's selection is
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
        if state.slots.get(id).is_some_and(AgentSlot::ready) {
            return Ok(());
        }
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
