//! User connections and project agent composition, independent of terminal widgets.
mod deletion;
pub(crate) mod model;
mod slot;
mod store;
mod ui;

use roomkit::model::{Access, ChannelId};
use roomkit::{ListBindingsOptions, RoomKit};
use roomkit_ai::ToolDefinition;
use roomkit_cli_roomkit::SessionConfig;
use roomkit_openai::OpenAiAgent;
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::ModelInfo;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

use crate::{config::Settings, storage::Storage};
use model::{Agent, Authentication, Project, Provider, Providers, TOOLS};
use slot::AgentSlot;
use store::Document;

pub(crate) struct Profiles {
    workspace: PathBuf,
    state_dir: PathBuf,
    pub(crate) diagnostics: Arc<crate::diagnostics::Diagnostics>,
    pub(crate) accounts: Arc<crate::accounts::Accounts>,
    pub(crate) resources: crate::agent::Resources,
    pub(crate) processes: Arc<crate::processes::Processes>,
    pub(crate) shell: Arc<crate::shell::Shell>,
    state: Mutex<State>,
}

struct State {
    providers: Document<Providers>,
    project: Document<Project>,
    slots: BTreeMap<String, AgentSlot>,
    tools: BTreeMap<String, Vec<ToolDefinition>>,
    pending: Option<ui::Pending>,
    pending_delete: Option<deletion::Pending>,
    setup_error: Option<String>,
}

impl Profiles {
    pub async fn load(
        settings: &Settings,
        storage: &Storage,
        api_key: String,
        resources: Option<crate::agent::Resources>,
    ) -> Result<Arc<Self>, String> {
        let diagnostics = crate::diagnostics::Diagnostics::open(
            storage.database.with_file_name("diagnostics.jsonl"),
            settings.debug,
        )
        .map_err(|_| "Cannot open the private diagnostic log")?;
        let edit_lease = store::edit_lease(&settings.state_dir)?;
        let mut providers =
            Document::<Providers>::load(settings.state_dir.join("connections.json"))?;
        let mut project =
            Document::<Project>::load(storage.database.with_file_name("agents.json"))?;
        if providers.value.schema != 1 {
            return Err("Unsupported profile file version".into());
        }
        if providers.value.profiles.len() > 128 || project.value.agents.len() > 64 {
            return Err("Too many provider or agent profiles".into());
        }
        for (id, provider) in &providers.value.profiles {
            provider.validate()?;
            if id != &provider.id {
                return Err("Invalid provider identity".into());
            }
        }
        for (id, agent) in &project.value.agents {
            agent.validate()?;
            if id != &agent.id {
                return Err("Invalid agent identity".into());
            }
        }
        let mut migrated = project.value.clone();
        if migrated.migrate()? {
            project.save(migrated)?;
        }
        if project.revision().is_none()
            && project.value.agents.is_empty()
            && !settings.base_url.is_empty()
        {
            let digest = format!(
                "{:x}",
                Sha256::digest(settings.workspace.as_os_str().as_encoded_bytes())
            );
            let id = format!("imported-{}", &digest[..12]);
            if !providers.value.profiles.contains_key(&id) {
                let provider = Provider {
                    id: id.clone(),
                    label: "Imported connection".into(),
                    base_url: settings.base_url.clone(),
                    authentication: if api_key.is_empty() {
                        Authentication::None
                    } else {
                        Authentication::Stored { key: api_key }
                    },
                    timeout_seconds: settings.provider_timeout_seconds,
                };
                provider.validate()?;
                let mut updated = providers.value.clone();
                updated.profiles.insert(id.clone(), provider);
                providers.save(updated)?;
            }
            let agent = Agent {
                id: "code".into(),
                label: "Code".into(),
                provider: id,
                model: settings.model.clone(),
                reasoning_effort: None,
                context_window: settings.context_window,
                instructions: String::new(),
                tools: TOOLS.iter().map(|s| (*s).into()).collect(),
            };
            agent.validate()?;
            let mut updated = project.value.clone();
            updated.selected = Some(agent.id.clone());
            updated.agents.insert(agent.id.clone(), agent);
            project.save(updated)?;
        }
        if project
            .value
            .selected
            .as_ref()
            .is_some_and(|id| !project.value.agents.contains_key(id))
        {
            return Err("Selected agent profile does not exist".into());
        }
        drop(edit_lease);
        let resources = match resources {
            Some(resources) => resources,
            None => crate::agent::Resources::new(
                &settings.workspace,
                storage.database.parent().ok_or("Missing state directory")?,
            )
            .await
            .map_err(|_| "Could not initialize workspace tools")?,
        };
        Ok(Arc::new(Self {
            shell: crate::shell::Shell::new(resources.processes.clone(), diagnostics.clone()),
            diagnostics,
            workspace: settings.workspace.clone(),
            state_dir: settings.state_dir.clone(),
            accounts: crate::accounts::Accounts::new(&settings.state_dir.join("auth"))?,
            processes: crate::processes::Processes::new(resources.processes.clone()),
            resources,
            state: Mutex::new(State {
                providers,
                project,
                slots: BTreeMap::new(),
                tools: BTreeMap::new(),
                pending: None,
                pending_delete: None,
                setup_error: None,
            }),
        }))
    }

    pub async fn initialize(&self, kit: &RoomKit<SqliteStore>) -> Result<ChannelId, String> {
        let mut state = self.state.lock().await;
        let id = state
            .project
            .value
            .selected
            .clone()
            .unwrap_or_else(|| "code".into());
        let channel = ChannelId::new(&id).map_err(|_| "Invalid agent ID")?;
        let assembled = if let Some(profile) = state.project.value.agents.get(&id) {
            let provider = state
                .providers
                .value
                .profiles
                .get(&profile.provider)
                .ok_or("Agent references an unknown provider")?;
            // A missing environment credential should not prevent opening its
            // settings. The view disables messages until configuration succeeds.
            match self.assemble(profile, provider).await {
                Ok(assembled) => Some(assembled),
                Err(error) => {
                    state.setup_error = Some(error);
                    None
                }
            }
        } else {
            None
        };
        let (agent, tools) = assembled.map_or((None, Vec::new()), |(a, t)| (Some(a), t));
        let configured = agent.is_some();
        let slot = AgentSlot::new(channel.clone(), agent);
        kit.register_channel(slot.clone())
            .map_err(|_| "Could not register the agent")?;
        state.slots.insert(id.clone(), slot);
        if configured {
            state.tools.insert(id, tools);
        }
        Ok(channel)
    }

    async fn assemble(
        &self,
        profile: &Agent,
        provider: &Provider,
    ) -> Result<(OpenAiAgent, Vec<ToolDefinition>), String> {
        profile.validate()?;
        provider.validate()?;
        let config = match &provider.authentication {
            Authentication::ChatGpt { account } => {
                let connected = self
                    .accounts
                    .auth
                    .accounts()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .find(|a| &a.id == account)
                    .ok_or("ChatGPT account not found; open /providers")?;
                if !connected.connected {
                    return Err("Sign in to this ChatGPT account with /providers".into());
                }
                if !connected.plan_usage {
                    return Err("Authorize ChatGPT plan usage with /providers".into());
                }
                roomkit_openai::OpenAiConfig::new(
                    roomkit_auth::CHATGPT_API_ROOT,
                    "",
                    &profile.model,
                )
                .with_api(roomkit_openai::OpenAiApi::Responses)
                .with_tool_namespace("functions", "Tools available to this assistant")
                .with_token_source(Arc::new(crate::accounts::Credential {
                    auth: self.accounts.auth.clone(),
                    account: account.clone(),
                }))
            }
            _ => roomkit_openai::OpenAiConfig::new(
                &provider.base_url,
                provider.authentication.key()?,
                &profile.model,
            ),
        }
        .with_timeout(std::time::Duration::from_secs(provider.timeout_seconds));
        let config = match &profile.reasoning_effort {
            Some(effort) => config.with_reasoning_effort(effort),
            None => config,
        };
        crate::agent::create(&self.workspace, &self.resources, profile, config)
            .await
            .map_err(
                |_| "Could not configure this agent; verify its provider, credential and workspace",
            )
            .map_err(String::from)
    }

    pub async fn configure_view(&self, view: &mut SessionConfig) {
        self.state.lock().await.view(view);
    }

    pub async fn tools(&self, id: &str) -> Vec<ToolDefinition> {
        self.state
            .lock()
            .await
            .tools
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    /// Only attach within a room whose terminal binding is already readable.
    pub async fn attach(
        &self,
        kit: &RoomKit<SqliteStore>,
        view: &SessionConfig,
        terminal: &ChannelId,
    ) -> Result<(), String> {
        let bindings = kit
            .list_bindings(
                &view.room,
                ListBindingsOptions {
                    organization_id: view.organization.clone(),
                },
            )
            .await
            .map_err(|_| "Cannot inspect session access")?;
        if !bindings.iter().any(|b| {
            &b.channel_id == terminal && matches!(b.access, Access::ReadOnly | Access::ReadWrite)
        }) {
            return Err("Session is not accessible to this terminal".into());
        }
        if !bindings.iter().any(|b| b.channel_id == view.agent) {
            kit.attach_channel(&view.room, &view.agent, Default::default())
                .await
                .map_err(|_| "Cannot attach the selected agent")?;
        }
        Ok(())
    }
}

impl State {
    fn view(&self, view: &mut SessionConfig) {
        view.actor_names = self
            .project
            .value
            .agents
            .values()
            .map(|a| (a.id.as_str().into(), a.label.clone()))
            .collect();
        if let Some(profile) = self.project.value.agents.get(view.agent.as_str()) {
            view.model = Some(ModelInfo {
                // Match the engine's usage identity, including Responses over OAuth.
                provider: "openai".into(),
                model: profile.model.clone(),
                effort: Some(
                    profile
                        .reasoning_effort
                        .clone()
                        .unwrap_or_else(|| "default".into()),
                ),
            });
            view.context_capacity = profile.context_window;
        } else {
            view.model = None;
            view.context_capacity = None;
        }
        view.submission_error = if self.tools.contains_key(view.agent.as_str()) {
            None
        } else {
            Some(self.setup_error.clone().unwrap_or_else(|| "Configure a connection with /providers, then create or select an agent with /agents".into()))
        };
    }

    fn ensure_slot(&mut self, kit: &RoomKit<SqliteStore>, id: &str) -> Result<(), String> {
        if !self.slots.contains_key(id) {
            let slot = AgentSlot::new(ChannelId::new(id).map_err(|_| "Invalid agent ID")?, None);
            kit.register_channel(slot.clone())
                .map_err(|_| "Could not register agent")?;
            self.slots.insert(id.into(), slot);
        }
        Ok(())
    }

    fn install(
        &mut self,
        kit: &RoomKit<SqliteStore>,
        id: &str,
        agent: OpenAiAgent,
        tools: Vec<ToolDefinition>,
    ) -> Result<(), String> {
        if let Some(slot) = self.slots.get(id) {
            slot.replace(agent);
        } else {
            let slot = AgentSlot::new(
                ChannelId::new(id).map_err(|_| "Invalid agent ID")?,
                Some(agent),
            );
            kit.register_channel(slot.clone())
                .map_err(|_| "Could not register agent")?;
            self.slots.insert(id.into(), slot);
        }
        self.tools.insert(id.into(), tools);
        self.setup_error = None;
        Ok(())
    }
}
