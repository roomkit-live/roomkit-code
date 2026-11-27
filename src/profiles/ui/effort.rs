//! Reasoning controls for the selected agent in one conversation.
use super::*;
use crate::local_models::EffortLevels;

impl Profiles {
    pub(super) async fn effort(
        &self,
        kit: &RoomKit<SqliteStore>,
        current: &SessionConfig,
        argument: &str,
    ) -> Result<CommandResult, String> {
        let mut state = self.state.lock().await;
        state.pending = None;
        state.pending_delete = None;
        state.providers.reload()?;
        state.reload_agents()?;
        let profile = active(&state, current)?;
        if !state.uses_llama_cpp(&profile) {
            return if argument.is_empty() {
                Ok(choices(
                    current,
                    "effort",
                    "Reasoning effort · supported levels depend on the model",
                    effort_choices(),
                ))
            } else {
                self.set_effort(kit, &mut state, current, argument).await
            };
        }
        let target = (
            profile.id.as_str(),
            profile.provider.as_str(),
            profile.model.as_str(),
        );
        if argument == "default" {
            return self.set_effort(kit, &mut state, current, argument).await;
        }
        if argument == "budget" {
            return Ok(budget_form(&mut state, current, &profile));
        }
        if argument.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            let _lease = store::edit_lease(&self.state_dir)?;
            return self
                .save_effort_budget(kit, &mut state, current.clone(), target, argument)
                .await;
        }
        let provider = state
            .providers
            .value
            .profiles
            .get(&profile.provider)
            .ok_or("Unknown provider")?;
        let local = self.local_models.provider(
            &profile,
            provider,
            &self.workspace,
            self.diagnostics.clone(),
        )?;
        drop(state);
        let levels = local.effort_levels(argument == "detect").await?;
        let mut state = self.state.lock().await;
        state.providers.reload()?;
        state.reload_agents()?;
        let profile = check_target(&state, current, target)?;
        if state.pending.is_some() {
            return Err("Another form was opened. Reopen /effort.".into());
        }
        match argument {
            "" | "detect" => Ok(local_menu(&mut state, current, &profile, levels)),
            level => match levels {
                EffortLevels::Native(ref offered) if offered.iter().any(|s| s == level) =>
                    self.set_effort(kit, &mut state, current, level).await,
                EffortLevels::NotLoaded => Err("Open /effort and load the model to discover its native levels, or set a token budget.".into()),
                _ => Err("This model did not advertise that effort level. Open /effort for its available controls.".into()),
            }
        }
    }

    async fn set_effort(
        &self,
        kit: &RoomKit<SqliteStore>,
        state: &mut State,
        current: &SessionConfig,
        level: &str,
    ) -> Result<CommandResult, String> {
        let _lease = store::edit_lease(&self.state_dir)?;
        let profile = active(state, current)?;
        if level != "default" && !model::EFFORTS.contains(&level) {
            return Err("Choose a supported reasoning effort or provider default".into());
        }
        let mut running = conversation_settings(state, current, &profile);
        running.effort = (level != "default").then(|| level.to_owned());
        if level == "default" && state.uses_llama_cpp(&profile) {
            running.thinking_budget = None;
        }
        self.keep_running(kit, state, current.clone(), running)
            .await
    }

    pub(super) async fn save_effort_budget(
        &self,
        kit: &RoomKit<SqliteStore>,
        state: &mut State,
        current: SessionConfig,
        target: (&str, &str, &str),
        text: &str,
    ) -> Result<CommandResult, String> {
        state.reload_agents()?;
        state.providers.reload()?;
        let profile = check_target(state, &current, target)?;
        let budget = match text.trim() {
            "" => None,
            n => Some(n.parse().map_err(
                |_| "Enter a nonnegative token count, or leave it empty to use the agent setting",
            )?),
        };
        crate::local_models::validate_thinking_budget(
            ProviderKind::LlamaCpp,
            budget,
            profile.max_output_tokens,
        )?;
        let mut running = conversation_settings(state, &current, &profile);
        running.thinking_budget = budget;
        self.keep_running(kit, state, current, running).await
    }
}

fn conversation_settings(
    state: &State,
    view: &SessionConfig,
    profile: &Agent,
) -> crate::sessions::Running {
    state
        .running
        .get(&(view.room.to_string(), profile.id.clone()))
        .cloned()
        .unwrap_or_else(|| conversation::own(profile))
}

fn active(state: &State, view: &SessionConfig) -> Result<Agent, String> {
    state
        .running_agent(view.room.as_str(), view.agent.as_str())
        .ok_or_else(|| "Configure an agent with /agents first".into())
}

fn check_target(
    state: &State,
    view: &SessionConfig,
    target: (&str, &str, &str),
) -> Result<Agent, String> {
    let agent = active(state, view)?;
    if (
        agent.id.as_str(),
        agent.provider.as_str(),
        agent.model.as_str(),
    ) != target
        || !state.uses_llama_cpp(&agent)
    {
        return Err("The local agent or model changed. Reopen /effort.".into());
    }
    Ok(agent)
}

fn budget_label(budget: Option<u32>) -> String {
    budget.map_or_else(|| "server default".into(), |n| format!("{n} tokens"))
}

fn budget_form(state: &mut State, view: &SessionConfig, agent: &Agent) -> CommandResult {
    let saved = state.budget_override(view.room.as_str(), &agent.id);
    let inherited = state.agent(&agent.id).and_then(|a| a.thinking_budget);
    let output = agent
        .max_output_tokens
        .unwrap_or_else(|| roomkit_providers_llamacpp::LlamaCppConfig::default().max_tokens);
    let form = FormSpec::new(
        uuid::Uuid::new_v4().to_string(),
        "effort",
        format!("Reasoning budget · {}", budget_label(agent.thinking_budget)),
        vec![field(
            "budget",
            "Reasoning tokens for this conversation",
            saved.map_or_else(String::new, |n| n.to_string()),
            &format!(
                "Output budget: {output} tokens. Enter a smaller number, 0 to disable reasoning, or leave empty to use the agent setting ({}).",
                budget_label(inherited)
            ),
            false,
            FieldKind::Text,
        )],
    );
    open_form(
        state,
        view,
        form,
        Target::ReasoningBudget {
            agent: agent.id.clone(),
            provider: agent.provider.clone(),
            model: agent.model.clone(),
        },
    )
}

fn local_menu(
    state: &mut State,
    view: &SessionConfig,
    agent: &Agent,
    levels: EffortLevels,
) -> CommandResult {
    let budget = choice(
        "budget",
        format!("Reasoning budget · {}", budget_label(agent.thinking_budget)),
        "Set a reasoning token limit for this conversation",
    );
    let (title, items) = match levels {
        EffortLevels::BudgetOnly => return budget_form(state, view, agent),
        EffortLevels::NotLoaded => (
            "Reasoning · model not loaded".into(),
            vec![
                budget,
                choice(
                    "detect",
                    "Load model to discover native levels",
                    "Starts the selected llama.cpp model",
                ),
            ],
        ),
        EffortLevels::Unreported => (
            "Reasoning · server did not report native levels".into(),
            vec![budget],
        ),
        EffortLevels::Native(levels) => {
            let mut items = vec![choice(
                "default",
                "Reset reasoning settings",
                "Use the default effort and inherit the agent’s token budget",
            )];
            items.extend(
                levels
                    .into_iter()
                    .map(|level| choice(level.clone(), level, "Native model effort")),
            );
            items.push(budget);
            (
                format!(
                    "Reasoning effort · {}",
                    agent.reasoning_effort.as_deref().unwrap_or("default")
                ),
                items,
            )
        }
    };
    choices(view, "effort", &title, items)
}
