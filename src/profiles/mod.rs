//! User connections and project agent composition, independent of terminal widgets.
mod catalog;
mod deletion;
mod mcp;
pub(crate) mod model;
pub(crate) mod sidebar;
mod skills;
mod slot;
mod startup;
mod store;
mod ui;

use roomkit::model::{Access, ChannelId};
use roomkit::{ListBindingsOptions, RoomKit};
use roomkit_ai::AiTool;
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::ModelInfo;
use roomkit_tui_engine::SessionConfig;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

use crate::{
    agent::{CodeAgent, Inventory},
    config::Settings,
    storage::Storage,
};
pub(crate) use mcp::McpTools;
use model::{Agent, Authentication, PROJECT_SCHEMA, Project, Provider, Providers, TOOLS};
use slot::AgentSlot;
use store::Document;

/// The name the OpenAI provider reports for the usage it measures, which a
/// view's model must carry for its measurements: Python's class name.
const PROVIDER_NAME: &str = "OpenAIAIProvider";

pub(crate) struct Profiles {
    pub(crate) policy: crate::permissions::Policy,
    pub(crate) plans: Arc<crate::plans::PlanView>,
    require_tool_intent: bool,
    workspace: PathBuf,
    state_dir: PathBuf,
    pub(crate) diagnostics: Arc<crate::diagnostics::Diagnostics>,
    pub(crate) accounts: Arc<crate::accounts::Accounts>,
    pub(crate) resources: crate::agent::Resources,
    pub(crate) processes: Arc<crate::processes::Processes>,
    pub(crate) images: Arc<crate::images::Images>,
    skill_catalogue: tokio::sync::RwLock<skills::Catalogue>,
    pub(crate) mcp_runtime: mcp::Runtime,
    state: Mutex<State>,
}

struct State {
    instructions: Arc<crate::instructions::Instructions>,
    /// The files the agents' tools change, which a slot settles as it closes.
    files: Arc<crate::files::Files>,
    policy: crate::permissions::Policy,
    mcp: Document<mcp::Servers>,
    pending_mcp: Option<mcp::Pending>,
    providers: Document<Providers>,
    project: Document<Project>,
    /// Agents shared by every project, beside the shared connections.
    global: Document<Project>,
    slots: BTreeMap<String, AgentSlot>,
    tools: BTreeMap<String, Vec<AiTool>>,
    /// What each configured agent's requests carry before their messages.
    compositions: BTreeMap<String, crate::agent::Composition>,
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
        let startup::Documents {
            mcp,
            providers,
            project,
            global,
        } = startup::load(settings, storage, api_key).await?;
        let resources = match resources {
            Some(resources) => resources,
            None => crate::agent::Resources::new(
                &settings.workspace,
                storage.database.parent().ok_or("Missing state directory")?,
                &settings.state_dir,
                &settings.inherit_env,
            )
            .await
            .map_err(|_| "Could not initialize workspace tools")?,
        };
        let skill_catalogue =
            skills::Catalogue::load(settings.workspace.clone(), settings.state_dir.clone()).await?;
        let policy = crate::permissions::Policy::new(settings.mode);
        let instructions = resources.instructions.clone();
        let files = resources.files.clone();
        Ok(Arc::new(Self {
            policy: policy.clone(),
            plans: crate::plans::PlanView::new((*resources.plans).clone()),
            require_tool_intent: settings.require_tool_intent,
            skill_catalogue: tokio::sync::RwLock::new(skill_catalogue),
            images: crate::images::Images::new(storage.database.with_file_name("attachments")),
            diagnostics,
            workspace: settings.workspace.clone(),
            state_dir: settings.state_dir.clone(),
            accounts: crate::accounts::Accounts::new(&settings.state_dir.join("auth"))?,
            processes: crate::processes::Processes::new(resources.processes.clone()),
            resources,
            mcp_runtime: mcp::Runtime::default(),
            state: Mutex::new(State {
                instructions,
                files,
                policy,
                mcp,
                pending_mcp: None,
                providers,
                project,
                global,
                slots: BTreeMap::new(),
                tools: BTreeMap::new(),
                compositions: BTreeMap::new(),
                pending: None,
                pending_delete: None,
                setup_error: None,
            }),
        }))
    }

    pub async fn initialize(&self, kit: &RoomKit<SqliteStore>) -> Result<ChannelId, String> {
        let mut state = self.state.lock().await;
        let id = state.selected().unwrap_or_else(|| "code".into());
        let channel = ChannelId::new(&id).map_err(|_| "Invalid agent ID")?;
        let assembled = if let Some(profile) = state.agent(&id) {
            let provider = state
                .providers
                .value
                .profiles
                .get(&profile.provider)
                .ok_or("Agent references an unknown provider")?;
            // A missing environment credential should not prevent opening its
            // settings. The view disables messages until configuration succeeds.
            match self.assemble(&profile, provider).await {
                Ok(assembled) => Some(assembled),
                Err(error) => {
                    state.setup_error = Some(error);
                    None
                }
            }
        } else {
            None
        };
        let (agent, inventory) =
            assembled.map_or((None, Inventory::default()), |(a, i)| (Some(a), i));
        let configured = agent.is_some();
        let slot = AgentSlot::new(
            channel.clone(),
            agent,
            self.policy.clone(),
            state.instructions.clone(),
            state.files.clone(),
        );
        kit.register_channel(slot.clone())
            .map_err(|_| "Could not register the agent")?;
        state.slots.insert(id.clone(), slot);
        if configured {
            state.compositions.insert(id.clone(), inventory.composition);
            state.tools.insert(id, inventory.tools);
        }
        Ok(channel)
    }

    async fn assemble(
        &self,
        profile: &Agent,
        provider: &Provider,
    ) -> Result<(CodeAgent, Inventory), String> {
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
                roomkit_openai::OpenAiConfig {
                    base_url: Some(roomkit_auth::CHATGPT_API_ROOT.into()),
                    api: roomkit_openai::OpenAiApi::Responses,
                    tool_namespace: Some((
                        "functions".into(),
                        "Tools available to this assistant".into(),
                    )),
                    token_source: Some(Arc::new(crate::accounts::Credential {
                        auth: self.accounts.auth.clone(),
                        account: account.clone(),
                    })),
                    ..roomkit_openai::OpenAiConfig::new("", &profile.model)
                }
            }
            _ => roomkit_openai::OpenAiConfig {
                base_url: Some(provider.base_url.clone()),
                ..roomkit_openai::OpenAiConfig::new(provider.authentication.key()?, &profile.model)
            },
        };
        let config = roomkit_openai::OpenAiConfig {
            timeout: std::time::Duration::from_secs(provider.timeout_seconds),
            vision: profile.vision,
            reasoning_effort: profile.reasoning_effort.clone(),
            // No cap unless the agent sets one: the endpoint decides how long
            // a reply may be, as each model has its own limit.
            max_tokens: profile.max_output_tokens,
            // The application sends no temperature, as before: the endpoint's
            // default applies, and a model that refuses one is never sent it.
            supports_custom_temperature: Some(false),
            ..config
        };
        let (skills, skill_roots) = {
            let catalogue = self.skill_catalogue.read().await;
            (
                catalogue.selected(&profile.skills),
                catalogue.roots().to_vec(),
            )
        };
        let extensions = crate::agent::Extensions {
            skills,
            skill_roots,
            mcp: self.mcp_runtime.selected(profile).await?,
            intent_required: self.require_tool_intent,
            policy: self.policy.clone(),
            images: self.images.clone(),
        };
        crate::agent::create(
            &self.workspace,
            &self.resources,
            profile,
            config,
            extensions,
        )
        .await
        .map_err(
            |_| "Could not configure this agent; verify its provider, credential and workspace",
        )
        .map_err(String::from)
    }

    pub async fn configure_view(&self, view: &mut SessionConfig) {
        self.state.lock().await.view(view);
    }

    pub async fn tools(&self, id: &str) -> Vec<AiTool> {
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
        let agent = self.agent(view.agent.as_str());
        let served = self.tools.contains_key(view.agent.as_str());
        view.can_attach_images = served && agent.as_ref().is_some_and(|a| a.vision);
        // Every agent's slot steers its turns: a message written while the
        // served agent works goes to its running turn.
        view.can_steer = served;
        view.actor_names = self
            .agents()
            .into_iter()
            .map(|(a, _)| (a.id.as_str().into(), a.label))
            .collect();
        if let Some(profile) = agent {
            // Match the engine's usage identity, including Responses over OAuth.
            view.model = Some(
                ModelInfo::new(PROVIDER_NAME, profile.model.clone()).with_effort(
                    profile
                        .reasoning_effort
                        .clone()
                        .unwrap_or_else(|| "default".into()),
                ),
            );
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
            let slot = AgentSlot::new(
                ChannelId::new(id).map_err(|_| "Invalid agent ID")?,
                None,
                self.policy.clone(),
                self.instructions.clone(),
                self.files.clone(),
            );
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
        agent: CodeAgent,
        inventory: Inventory,
    ) -> Result<(), String> {
        if let Some(slot) = self.slots.get(id) {
            slot.replace(agent);
        } else {
            let slot = AgentSlot::new(
                ChannelId::new(id).map_err(|_| "Invalid agent ID")?,
                Some(agent),
                self.policy.clone(),
                self.instructions.clone(),
                self.files.clone(),
            );
            kit.register_channel(slot.clone())
                .map_err(|_| "Could not register agent")?;
            self.slots.insert(id.into(), slot);
        }
        self.compositions.insert(id.into(), inventory.composition);
        self.tools.insert(id.into(), inventory.tools);
        self.setup_error = None;
        Ok(())
    }
}
