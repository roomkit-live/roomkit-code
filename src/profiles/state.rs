//! What the profiles' state does with the engine: the agents' slots, and
//! what a view shows of the selected one.
use super::*;

impl State {
    /// The name of the connection `agent` runs on, its identity without one.
    pub(super) fn provider_label<'a>(&'a self, agent: &'a Agent) -> &'a str {
        self.providers
            .value
            .profiles
            .get(&agent.provider)
            .map_or(agent.provider.as_str(), |provider| provider.label.as_str())
    }

    pub(super) fn view(&self, view: &mut SessionConfig) {
        // The agent as it runs in the view's conversation.
        let agent = self.running_agent(view.room.as_str(), view.agent.as_str());
        let served = self.tools.contains_key(view.agent.as_str());
        view.can_attach_images = false;
        view.actor_names = self
            .agents()
            .into_iter()
            .map(|(a, _)| (a.id.as_str().into(), a.label))
            .collect();
        // What the application tells its views in a conversation, such as a
        // compaction's line, comes from the terminal's channel.
        view.actor_names
            .insert(crate::names::TERMINAL.into(), env!("CARGO_PKG_NAME").into());
        if let Some(profile) = agent {
            let kind = self
                .providers
                .value
                .profiles
                .get(&profile.provider)
                .map(|p| p.kind)
                .unwrap_or_default();
            view.can_attach_images =
                served && profile.vision && kind != crate::providers::ProviderKind::LlamaCpp;
            // Match the engine's usage identity, including Responses over OAuth.
            let mut model = ModelInfo::new(kind.name(), &profile.model)
                .with_effort(profile.reasoning_effort.as_deref().unwrap_or("default"));
            if kind == crate::providers::ProviderKind::LlamaCpp {
                model.context_limit_hint = Some("/model → Agent context capacity".into());
                if let Some(label) = crate::local_models::display_name(&profile.model) {
                    model = model.with_display_name(label);
                }
            }
            view.model = Some(model);
            view.context_capacity = profile.context_window.or_else(|| {
                (kind == crate::providers::ProviderKind::LlamaCpp)
                    .then_some(crate::local_models::DEFAULT_CONTEXT.into())
            });
            view.pricing = self.pricing(&profile);
        } else {
            view.model = None;
            view.context_capacity = None;
            view.pricing = None;
        }
        let key = (view.room.to_string(), view.agent.to_string());
        view.submission_error = if let Some(reason) = self.unfitted.get(&key) {
            Some(reason.clone())
        } else if self.is_remote(view.agent.as_str())
            || self.tools.contains_key(view.agent.as_str())
        {
            None
        } else {
            Some(self.setup_error.clone().unwrap_or_else(|| "Configure a connection with /providers, then create or select an agent with /agents".into()))
        };
    }

    pub(super) fn pricing(&self, agent: &Agent) -> Option<roomkit_ai::ModelPricing> {
        let connection = self.providers.value.profiles.get(&agent.provider)?;
        list_price(connection, &agent.model, self.configured_price.as_ref())
    }

    /// Registers agent `id`'s slot, holding `agent`, with the engine, which
    /// serves it the delegation tools.
    pub(super) fn register(
        &mut self,
        kit: &RoomKit<SqliteStore>,
        id: &ChannelId,
        agent: Option<CodeAgent>,
    ) -> Result<(), String> {
        let slot = AgentSlot::new(
            id.clone(),
            agent,
            self.policy.clone(),
            self.instructions.clone(),
            self.files.clone(),
            self.compactions.clone(),
            self.roster.clone(),
        )
        .with_turn_ends(self.turn_ends.clone())
        .with_human_input(self.asking.clone());
        kit.register_channel(slot.clone())
            .map_err(|_| "Could not register the agent")?;
        // Kept as soon as the engine holds it, which never registers it twice.
        self.slots.insert(id.as_str().into(), slot);
        if let Some(handler) = &self.delegation {
            crate::delegation::serve(kit, handler, id)
                .map_err(|_| "Could not serve the agent its delegation tools")?;
        }
        Ok(())
    }

    pub(super) fn ensure_slot(
        &mut self,
        kit: &RoomKit<SqliteStore>,
        id: &str,
    ) -> Result<(), String> {
        if !self.slots.contains_key(id) {
            let channel = ChannelId::new(id).map_err(|_| "Invalid agent ID")?;
            self.register(kit, &channel, None)?;
        }
        Ok(())
    }

    pub(super) fn install(
        &mut self,
        kit: &RoomKit<SqliteStore>,
        id: &str,
        agent: CodeAgent,
        inventory: Inventory,
    ) -> Result<(), String> {
        self.put(kit, id, agent, inventory)?;
        self.setup_error = None;
        Ok(())
    }

    /// Puts `agent` in agent `id`'s slot, registering the slot first if needed.
    pub(super) fn put(
        &mut self,
        kit: &RoomKit<SqliteStore>,
        id: &str,
        agent: CodeAgent,
        inventory: Inventory,
    ) -> Result<(), String> {
        if let Some(slot) = self.slots.get(id) {
            slot.replace(agent);
        } else {
            let channel = ChannelId::new(id).map_err(|_| "Invalid agent ID")?;
            self.register(kit, &channel, Some(agent))?;
        }
        self.compositions.insert(id.into(), inventory.composition);
        self.tools.insert(id.into(), inventory.tools);
        Ok(())
    }
}
