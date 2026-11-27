//! The context window a local agent allocates to its llama.cpp server.
use super::*;

pub(super) fn capacity(value: &str) -> Result<Option<u64>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let count = match value.strip_suffix(['k', 'K']) {
        Some(n) => n.parse::<u64>().ok().and_then(|n| n.checked_mul(1024)),
        None => value.parse().ok(),
    };
    count.filter(|n| (1..=1_000_000_000).contains(n)).map(Some)
        .ok_or_else(|| "Context capacity must be a positive token count, such as 131072 or 128K (maximum 1000000000)".into())
}

impl Profiles {
    pub(super) fn local_context_form(
        &self,
        state: &mut State,
        current: &SessionConfig,
    ) -> Result<CommandResult, String> {
        let agent = state.agent(current.agent.as_str()).ok_or("Unknown agent")?;
        let field = field(
            "context",
            "Context capacity",
            agent
                .context_window
                .map_or_else(String::new, |n| n.to_string()),
            "Tokens for this agent’s local models (128K = 131072). Empty uses the 16384-token default.",
            false,
            FieldKind::Text,
        );
        let form = FormSpec::new(
            uuid::Uuid::new_v4().to_string(),
            "model",
            format!("{} · context capacity", agent.label),
            vec![field],
        );
        Ok(open_form(
            state,
            current,
            form,
            Target::LocalContext {
                agent: agent.id,
                provider: agent.provider,
            },
        ))
    }

    pub(super) async fn save_local_context(
        &self,
        kit: &RoomKit<SqliteStore>,
        state: &mut State,
        view: &SessionConfig,
        id: &str,
        provider: &str,
        value: &str,
    ) -> Result<(), String> {
        state.reload_agents()?;
        state.providers.reload()?;
        let mut agent = state.agent(id).ok_or("Unknown agent")?;
        if id != view.agent.as_str() || agent.provider != provider || !state.uses_llama_cpp(&agent)
        {
            return Err("The local agent changed. Reopen /model.".into());
        }
        agent.context_window = capacity(value)?;
        agent.validate()?;
        let provider = state
            .providers
            .value
            .profiles
            .get(provider)
            .ok_or("Unknown provider")?;
        let (runtime, tools) = self.assemble(&agent, provider).await?;
        state.update_agent(agent)?;
        self.installed(kit, state, id, runtime, tools).await
    }
}
