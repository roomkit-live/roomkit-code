//! Choosing a model among those a connection serves: right after a
//! connection is added, for a new agent and for an existing one in
//! `/agents`, and for the current agent with `/model`.
use roomkit_tui::protocol::{Choice, Event};
use roomkit_tui_engine::CommandResult;

use super::discovery::discover;
use super::ui::{choice, choices};
use super::*;

/// One model a connection serves: its ID, the name it is shown by, and its
/// reported capacity when its server tells it.
struct Served {
    id: String,
    name: String,
    capacity: Option<u64>,
}

impl Profiles {
    /// The models `provider` serves: a ChatGPT account's, as the account
    /// lists them; any other connection's, as its server's `GET /models`
    /// does.
    async fn served(&self, provider: &Provider) -> Result<Vec<Served>, String> {
        if let Authentication::ChatGpt { account } = &provider.authentication {
            let models = self
                .accounts
                .auth
                .models(account)
                .await
                .map_err(|e| e.to_string())?;
            if models.is_empty() {
                return Err("This ChatGPT account returned no available models".into());
            }
            return Ok(models
                .into_iter()
                .map(|m| Served {
                    id: m.id,
                    name: m.name,
                    capacity: None,
                })
                .collect());
        }
        Ok(discover(provider)
            .await?
            .into_iter()
            .map(|(id, capacity)| Served {
                name: id.clone(),
                id,
                capacity,
            })
            .collect())
    }

    /// The models `provider` serves, each opening the form of a new agent
    /// that uses it, under `title`, then the choice to type another ID.
    pub(super) async fn creation_choices(
        &self,
        current: &SessionConfig,
        provider: &Provider,
        title: &str,
    ) -> Result<CommandResult, String> {
        let served = self.served(provider).await?;
        let entries = served
            .into_iter()
            .map(|m| {
                choice(
                    format!("model:{}:{}", provider.id, m.id),
                    m.name,
                    m.capacity.map_or_else(
                        || "Create agent with this model".into(),
                        |n| format!("Reported capacity: {n} tokens · create agent"),
                    ),
                )
            })
            .chain(std::iter::once(typed_model(&provider.id)))
            .collect();
        Ok(choices(current, "providers", title, entries))
    }

    /// `+ Add agent` in `/agents`: the connection to create it on, then that
    /// connection's models; the models at once when there is one connection,
    /// or when `connection` names it.
    pub(super) async fn new_agent(
        &self,
        state: tokio::sync::MutexGuard<'_, State>,
        current: SessionConfig,
        connection: &str,
    ) -> Result<CommandResult, String> {
        let connections = &state.providers.value.profiles;
        let only = if connections.len() == 1 {
            connections.keys().next().map(String::as_str)
        } else {
            None
        };
        let Some(id) = Some(connection).filter(|c| !c.is_empty()).or(only) else {
            if connections.is_empty() {
                return Err("Add a connection with /providers first".into());
            }
            let entries = connections
                .values()
                .map(|p| choice(format!("new:{}", p.id), &p.label, &p.id))
                .collect();
            return Ok(choices(
                &current,
                "agents",
                "New agent · choose its connection",
                entries,
            ));
        };
        let provider = connections.get(id).ok_or("Unknown provider")?.clone();
        // A listing reads a server: the other profile commands do not wait on it.
        drop(state);
        let title = "New agent · choose its model";
        Ok(self
            .creation_choices(&current, &provider, title)
            .await
            .unwrap_or_else(|error| unlisted(&current, &provider.id, error)))
    }

    /// `Change model` of agent `id` in `/agents`: the models its connection
    /// serves, each opening the agent's form with that model.
    pub(super) async fn agent_models(
        &self,
        state: tokio::sync::MutexGuard<'_, State>,
        current: SessionConfig,
        id: &str,
    ) -> Result<CommandResult, String> {
        let agent = state.agent(id).ok_or("Unknown agent")?;
        let provider = state
            .providers
            .value
            .profiles
            .get(&agent.provider)
            .ok_or("Unknown provider")?
            .clone();
        // A listing reads a server: the other profile commands do not wait on it.
        drop(state);
        let served = self
            .served(&provider)
            .await
            .map_err(|error| format!("{error}; Edit agent sets a model by its ID"))?;
        Ok(choices(
            &current,
            "agents",
            &format!("{} · choose a model", agent.label),
            served
                .into_iter()
                .map(|m| {
                    let value = format!("use-model:{}:{}", agent.id, m.id);
                    listed_choice(value, m, &agent.model)
                })
                .collect(),
        ))
    }

    /// What follows saving a new connection no agent uses yet: the models it
    /// serves, to create an agent with one, as a ChatGPT account's sign-in
    /// offers them; a notice when they cannot be listed, the connection
    /// staying saved.
    pub(super) async fn after_connecting(
        &self,
        current: &SessionConfig,
        provider: &Provider,
    ) -> CommandResult {
        let title = "Connected · choose a model to create an agent";
        self.creation_choices(current, provider, title)
            .await
            .unwrap_or_else(|error| {
                unlisted(current, &provider.id, format!("Connection saved. {error}"))
            })
    }

    /// `/model`: the models the current agent's connection serves, or, given
    /// one's ID, that model for the current agent. An ID is any the server
    /// serves, listed or not; a listing that fails leaves it to be typed.
    pub(super) async fn model(
        &self,
        kit: &RoomKit<SqliteStore>,
        mut state: tokio::sync::MutexGuard<'_, State>,
        current: SessionConfig,
        id: &str,
    ) -> Result<CommandResult, String> {
        if !id.is_empty() {
            return self
                .change_current(kit, &mut state, current, |agent| agent.model = id.into())
                .await;
        }
        let agent = state
            .agent(current.agent.as_str())
            .ok_or("Configure an agent with /agents first")?;
        let provider = state
            .providers
            .value
            .profiles
            .get(&agent.provider)
            .ok_or("Unknown provider")?
            .clone();
        // A listing reads a server: the other profile commands do not wait on it.
        drop(state);
        let served = self
            .served(&provider)
            .await
            .map_err(|error| format!("{error}; /model <ID> sets a model by its ID"))?;
        Ok(choices(
            &current,
            "model",
            &format!("Model · {} · {}", agent.label, provider.label),
            served
                .into_iter()
                .map(|m| listed_choice(m.id.clone(), m, &agent.model))
                .collect(),
        ))
    }

    /// Saves `change` to the current agent and reinstalls it, as `/effort`
    /// and `/model` do.
    pub(super) async fn change_current(
        &self,
        kit: &RoomKit<SqliteStore>,
        state: &mut State,
        current: SessionConfig,
        change: impl FnOnce(&mut Agent),
    ) -> Result<CommandResult, String> {
        let mut agent = state
            .agent(current.agent.as_str())
            .ok_or("Configure an agent with /agents first")?;
        change(&mut agent);
        agent.validate()?;
        let provider = state
            .providers
            .value
            .profiles
            .get(&agent.provider)
            .ok_or("Unknown provider")?;
        let (runtime, tools) = self.assemble(&agent, provider).await?;
        state.update_agent(agent)?;
        state.install(kit, current.agent.as_str(), runtime, tools)?;
        let mut next = current;
        state.view(&mut next);
        Ok(CommandResult::Switch(Box::new(next)))
    }
}

/// The choice that opens the form of a new agent on connection `id` with
/// no model, to type one its list does not show.
fn typed_model(id: &str) -> Choice {
    choice(
        format!("typed:{id}"),
        "Type a model ID",
        "For a model this list does not show",
    )
}

/// What a listing of connection `id` that failed answers: `notice`, saying
/// why, and the one choice to type the model's ID.
fn unlisted(current: &SessionConfig, id: &str, notice: String) -> CommandResult {
    let conversation = current.room.as_str();
    CommandResult::Events(vec![
        Event::Notice {
            conversation: conversation.into(),
            text: notice,
        },
        Event::Choices {
            conversation: conversation.into(),
            command: "providers".into(),
            title: "No model list · type the model ID".into(),
            choices: vec![typed_model(id)],
        },
    ])
}

/// `served` as a choice of `value` for an agent, its `current` model marked.
fn listed_choice(value: String, served: Served, current: &str) -> Choice {
    let description = if served.id == current {
        "Current model".into()
    } else {
        served.capacity.map_or_else(
            || "Use this model".into(),
            |n| format!("Reported capacity: {n} tokens"),
        )
    };
    choice(value, served.name, description)
}
