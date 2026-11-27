//! Assembling an agent from its profile, its connection, its skills and
//! its MCP servers as they stand.
use super::*;

impl Profiles {
    /// Assembles `profile` on `provider`, as the agent's own configuration.
    pub(super) async fn assemble(
        &self,
        profile: &Agent,
        provider: &Provider,
    ) -> Result<(CodeAgent, Inventory), String> {
        self.assemble_for(profile, provider, None).await
    }

    /// Assembles `profile` on `provider`: the agent's own configuration, or
    /// the one it runs with in `conversation`, which keeps a model or an
    /// effort of its own for it.
    pub(super) async fn assemble_for(
        &self,
        profile: &Agent,
        provider: &Provider,
        conversation: Option<&str>,
    ) -> Result<(CodeAgent, Inventory), String> {
        self.assemble_channel(profile, provider, conversation, &profile.id)
            .await
    }

    pub(super) async fn assemble_channel(
        &self,
        profile: &Agent,
        provider: &Provider,
        conversation: Option<&str>,
        channel: &str,
    ) -> Result<(CodeAgent, Inventory), String> {
        let inventory = self
            .active_pool()
            .map(|p| p.inventory())
            .or_else(|| self.executor_inventory.clone());
        self.assemble_with_inventory(profile, provider, conversation, channel, inventory)
            .await
    }

    pub(super) async fn assemble_with_inventory(
        &self,
        profile: &Agent,
        provider: &Provider,
        conversation: Option<&str>,
        channel: &str,
        inventory: Option<crate::remote::pool::Inventory>,
    ) -> Result<(CodeAgent, Inventory), String> {
        profile.validate()?;
        crate::local_models::validate_thinking_budget(
            provider.kind,
            profile.thinking_budget,
            profile.max_output_tokens,
        )?;
        provider.validate()?;
        let provider_instance = self.generation_provider(profile, provider)?;
        let (skills, skill_roots) = {
            let catalogue = self.skill_catalogue.read().await;
            (
                catalogue.selected(&profile.skills)?,
                catalogue.roots().to_vec(),
            )
        };
        let extensions = crate::agent::Extensions {
            project_memory: self.project_memory.clone(),
            history: crate::project_memory::History::for_agent(
                self.conversation_store.as_ref(),
                channel,
            ),
            executor_inventory: inventory,
            skills,
            skill_roots,
            mcp: self.mcp_runtime.selected(profile).await?,
            intent_required: self.require_tool_intent,
            max_tool_rounds: self.max_tool_rounds,
            unattended: self.unattended,
            planning: self.planning,
            delegation: !self.unattended,
            policy: self.policy.clone(),
            images: self.images.clone(),
            ceiling: ceiling(profile, provider.kind),
            human_input: self.asking.clone(),
            conversation: conversation.map(str::to_owned),
        };
        let created = crate::agent::create(
            &self.workspace,
            channel,
            &self.resources,
            profile,
            provider_instance,
            extensions,
        )
        .await
        .map_err(
            |_| "Could not configure this agent; verify its provider, credential and workspace",
        )?;
        let compactions = &self.resources.compactions;
        let bounded = ceiling(profile, provider.kind).is_some();
        match conversation {
            Some(room) => compactions.set_bounded_in(room, &profile.id, bounded),
            None => compactions.set_bounded(&profile.id, bounded),
        }
        Ok(created)
    }
}
