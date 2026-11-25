//! The menus of `/providers`, `/agents` and `/effort`: what each entry
//! offers.
use super::*;

pub(super) fn effort_choices() -> Vec<Choice> {
    std::iter::once(choice(
        "default",
        "Provider default",
        "Let the model use its default effort",
    ))
    .chain(
        model::EFFORTS
            .iter()
            .map(|e| choice(*e, *e, "Choose a level supported by this model")),
    )
    .collect()
}

/// What a connection's entry in `/providers` offers.
pub(super) fn provider_menu(
    current: &SessionConfig,
    id: &str,
    provider: &Provider,
) -> CommandResult {
    let entries = if let Authentication::ChatGpt { account } = &provider.authentication {
        vec![
            choice(
                Action::ChatGpt(&format!("account:{account}")).to_string(),
                "Manage ChatGPT account",
                "Choose models, sign in or sign out",
            ),
            choice(
                Action::Delete {
                    command: "providers",
                    id,
                }
                .to_string(),
                "Delete provider",
                "Remove this connection; account sign-out is separate",
            ),
        ]
    } else {
        vec![
            choice(
                Action::EditProvider(id).to_string(),
                "Edit connection",
                if provider.subscription().is_some() {
                    "Name, API key and timeout"
                } else {
                    "URL, authentication and timeout"
                },
            ),
            choice(
                Action::TestProvider(id).to_string(),
                "Test connection and list models",
                "Read-only GET /models; no conversation is sent",
            ),
            choice(
                Action::Delete {
                    command: "providers",
                    id,
                }
                .to_string(),
                "Delete provider",
                "Remove this shared connection if no agents use it",
            ),
        ]
    };
    choices(current, "providers", &provider.label, entries)
}

/// What an agent's entry in `/agents` offers.
pub(super) fn agent_menu(current: &SessionConfig, id: &str, agent: &Agent) -> CommandResult {
    let entries = vec![
        choice(
            Action::SelectAgent(id).to_string(),
            "Select agent",
            format!("{} · {}", agent.provider, agent.model),
        ),
        choice(
            Action::AgentModels(id).to_string(),
            "Change model",
            "Choose among the models its connection serves",
        ),
        choice(
            Action::EditAgent(id).to_string(),
            "Edit agent",
            "Provider, model, effort, instructions and tools",
        ),
        choice(
            Action::Delete {
                command: "agents",
                id,
            }
            .to_string(),
            "Delete agent",
            "Remove this profile; keep conversation history",
        ),
    ];
    choices(current, "agents", &agent.label, entries)
}

pub(in crate::profiles) fn provider_choices(
    state: &State,
    current: &SessionConfig,
) -> CommandResult {
    let mut entries = vec![choice(
        Action::AddProvider.to_string(),
        "+ Add provider",
        "OpenAI-compatible or Anthropic connection",
    )];
    entries.extend(super::subscriptions::ALL.iter().map(|subscription| {
        let offer = &subscription.offer;
        choice(offer.action, offer.label, offer.description)
    }));
    entries.extend(state.providers.value.profiles.values().map(|p| {
        choice(
            Action::OpenProvider(&p.id).to_string(),
            &p.label,
            format!("{} · {}", p.id, p.base_url),
        )
    }));
    choices(
        current,
        "providers",
        "Providers · shared across your projects",
        entries,
    )
}
pub(super) fn agent_choices(state: &State, current: &SessionConfig) -> CommandResult {
    let mut entries = vec![choice(
        "new",
        "+ Add agent",
        "Choose its connection and model, then its instructions and tools",
    )];
    for (agent, scope) in state.agents() {
        entries.push(choice(
            Action::OpenAgent(&agent.id).to_string(),
            format!(
                "{}{}",
                if current.agent.as_str() == agent.id {
                    "● "
                } else {
                    ""
                },
                agent.label
            ),
            format!(
                "{} · {} · {} · {}",
                agent.id,
                scope.value(),
                agent.provider,
                agent.model
            ),
        ));
    }
    choices(
        current,
        "agents",
        "Agents · global and this project's · shared conversation",
        entries,
    )
}
