//! Confirmed profile removal without deleting room history or shared accounts.

use super::*;
use roomkit_tui_engine::CommandResult;
use ui::{choice, choices};

pub(super) struct Pending {
    token: String,
    command: String,
    id: String,
    room: String,
    selected: String,
    providers_revision: Option<Vec<u8>>,
    project_revision: Option<Vec<u8>>,
    global_revision: Option<Vec<u8>>,
    /// Deleting a global agent needs the global file unchanged.
    global: bool,
}

impl Profiles {
    pub(super) async fn delete(
        &self,
        kit: &RoomKit<SqliteStore>,
        terminal: &ChannelId,
        command: &str,
        arguments: &str,
        current: SessionConfig,
    ) -> Result<CommandResult, String> {
        let (action, id) = arguments.split_once(':').ok_or("Invalid delete action")?;
        let mut state = self.state.lock().await;
        state.pending = None;
        if action == "confirm-delete" {
            let pending = state
                .pending_delete
                .take()
                .ok_or("Reopen the profile to confirm deletion")?;
            if pending.token != id
                || pending.command != command
                || pending.room != current.room.as_str()
                || pending.selected != current.agent.as_str()
            {
                return Err("This confirmation does not belong to the current view".into());
            }
            let _lease = store::edit_lease(&self.state_dir)?;
            state.providers.reload()?;
            state.reload_agents()?;
            // A provider's references are rechecked below, global agents
            // included; an agent's deletion needs the agent unchanged.
            if state.providers.revision() != pending.providers_revision
                || state.project.revision() != pending.project_revision
                || (pending.global && state.global.revision() != pending.global_revision)
            {
                return Err(
                    "Profiles changed; reopen the profile and confirm deletion again".into(),
                );
            }
            if command == "providers" {
                self.check_provider_references(&state, &pending.id)?;
                let mut providers = state.providers.value.clone();
                providers
                    .profiles
                    .remove(&pending.id)
                    .ok_or("Unknown provider")?;
                state.providers.save(providers)?;
                return Ok(ui::provider_choices(&state, &current));
            }
            return self
                .remove_agent(kit, terminal, &mut state, &pending.id, current)
                .await;
        }

        state.pending_delete = None;
        state.providers.reload()?;
        state.reload_agents()?;
        let (label, description) = if command == "providers" {
            let provider = state
                .providers
                .value
                .profiles
                .get(id)
                .ok_or("Unknown provider")?;
            self.check_provider_references(&state, id)?;
            (
                provider.label.clone(),
                match provider.account_managed() {
                    Some(subscription) => format!(
                        "Remove this shared connection. The {} account stays connected; sign out separately from the account menu.",
                        subscription.name
                    ),
                    None => "Remove this shared connection and its stored API key, if any.".into(),
                },
            )
        } else {
            let agent = state.agent(id).ok_or("Unknown agent")?;
            let others = state.agents();
            let next = if current.agent.as_str() != id {
                "The active agent stays selected.".into()
            } else if let Some((next, _)) = others.iter().find(|(a, _)| a.id != id) {
                format!("Switch to {} ({}).", next.label, next.id)
            } else {
                "Return to setup; add an agent to send new messages.".into()
            };
            let reach = if state.scope(id) == Some(catalog::Scope::Global) {
                "This global agent leaves every project. "
            } else {
                ""
            };
            (
                agent.label.clone(),
                format!("{reach}Keep all conversation history. {next}"),
            )
        };
        let token = uuid::Uuid::new_v4().to_string();
        state.pending_delete = Some(Pending {
            token: token.clone(),
            command: command.into(),
            id: id.into(),
            room: current.room.to_string(),
            selected: current.agent.to_string(),
            providers_revision: state.providers.revision(),
            project_revision: state.project.revision(),
            global_revision: state.global.revision(),
            global: command == "agents" && state.scope(id) == Some(catalog::Scope::Global),
        });
        Ok(choices(
            &current,
            command,
            &format!("Delete {label} ({id})?"),
            vec![
                choice(format!("open:{id}"), "Cancel", "Keep this profile"),
                choice(format!("confirm-delete:{token}"), "Delete", description),
            ],
        ))
    }

    async fn remove_agent(
        &self,
        kit: &RoomKit<SqliteStore>,
        terminal: &ChannelId,
        state: &mut State,
        id: &str,
        mut current: SessionConfig,
    ) -> Result<CommandResult, String> {
        state.scope(id).ok_or("Unknown agent")?;
        let remaining: Vec<String> = state
            .agents()
            .into_iter()
            .map(|(agent, _)| agent.id)
            .filter(|agent| agent != id)
            .collect();
        let selected = current.agent.as_str() == id;
        let next = if selected {
            remaining.first().cloned()
        } else {
            Some(current.agent.to_string()).filter(|selected| remaining.contains(selected))
        };
        let replacement = if selected {
            current.agent = ChannelId::new(next.as_deref().unwrap_or("code"))
                .map_err(|_| "Invalid agent ID")?;
            state.ensure_slot(kit, current.agent.as_str())?;
            self.attach(kit, &current, terminal).await?;
            match next.as_ref().and_then(|id| state.agent(id)) {
                Some(agent) => Some(match state.providers.value.profiles.get(&agent.provider) {
                    Some(provider) => self.assemble(&agent, provider).await,
                    None => Err("Configure the selected agent's provider with /agents".into()),
                }),
                None => None,
            }
        } else {
            None
        };

        state.remove_agent(id, next.as_deref())?;
        state.disable(id);
        if selected {
            state.disable(current.agent.as_str());
            state.setup_error = None;
            match replacement {
                Some(Ok((agent, tools))) => {
                    self.installed(kit, state, current.agent.as_str(), agent, tools)
                        .await?;
                }
                Some(Err(error)) => state.setup_error = Some(error),
                None => {}
            }
            // The person confirmed the switch: the conversation designates
            // the agent that replaces the deleted one. Without one, or if
            // the record fails, it keeps the deleted agent, and opens with no
            // agent rather than with another.
            if next.is_some() {
                let _ = self.designate(kit, &current).await;
            }
        }
        self.settle_in(kit, state, &mut current).await;
        Ok(CommandResult::Switch(Box::new(current)))
    }

    /// Recheck every project under the shared edit lease at confirmation time.
    /// An unknown/corrupt profile file prevents deletion rather than orphaning it.
    fn check_provider_references(&self, state: &State, id: &str) -> Result<(), String> {
        // Global agents are shared by every project; state.reload_agents()
        // reread them under this lease.
        let mut references: Vec<String> = state
            .global
            .value
            .agents
            .values()
            .filter(|agent| agent.provider == id)
            .map(|agent| format!("{} ({}), a global agent", agent.label, agent.id))
            .collect();
        let entries = std::fs::read_dir(self.state_dir.join("projects"))
            .map_err(|_| "Cannot inspect project profiles before deleting this provider")?;
        for entry in entries {
            let entry = entry.map_err(|_| "Cannot inspect project profiles")?;
            let kind = entry
                .file_type()
                .map_err(|_| "Cannot inspect project profiles")?;
            if kind.is_symlink() {
                return Err(
                    "A project profile directory is a symlink; cannot verify provider references"
                        .into(),
                );
            }
            if !kind.is_dir() {
                continue;
            }
            let path = entry.path().join("agents.json");
            let project = Document::<Project>::load(path.clone())
                .map_err(|_| "Cannot read a project's agent profiles; repair them before deleting this provider")?;
            if !matches!(project.value.schema, 4 | PROJECT_SCHEMA) {
                return Err(format!(
                    "{} has an unsupported version; cannot verify provider references",
                    path.display()
                ));
            }
            for agent in project
                .value
                .agents
                .values()
                .filter(|agent| agent.provider == id)
            {
                let location = if path == state.project.path() {
                    "this project".into()
                } else {
                    format!(
                        "project {}",
                        entry
                            .file_name()
                            .to_string_lossy()
                            .chars()
                            .take(12)
                            .collect::<String>()
                    )
                };
                references.push(format!("{} ({}) in {location}", agent.label, agent.id));
            }
        }
        if !references.is_empty() {
            references.sort();
            let count = references.len();
            references.truncate(8);
            return Err(format!(
                "Provider is used by {count} agent(s): {}. Change their provider or delete those agents first.",
                references.join("; ")
            ));
        }
        Ok(())
    }
}

impl State {
    fn disable(&mut self, id: &str) {
        if let Some(slot) = self.slots.get(id) {
            slot.disable();
        }
        self.tools.remove(id);
        self.compositions.remove(id);
        // What the conversations keep for it is read again when it is shown.
        self.running.retain(|(_, agent), _| agent != id);
        self.unfitted.retain(|(_, agent), _| agent != id);
    }
}
