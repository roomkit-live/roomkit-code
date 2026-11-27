//! User connections and project agent composition, independent of terminal widgets.
mod assembly;
mod catalog;
mod conversation;
mod deletion;
mod discovery;
mod executors;
pub(crate) mod mcp;
pub(crate) mod model;
mod model_choice;
mod provider;
mod reasoning;
pub(crate) mod sidebar;
mod skills;
mod slot;
mod startup;
mod state;
mod store;
mod subscription_form;
mod subscriptions;
mod targets;
mod ui;
mod views;
mod worker;

use roomkit::model::{Access, ChannelId};
use roomkit::{ListBindingsOptions, RoomKit};
use roomkit_ai::AiTool;
use roomkit_providers_sqlite::SqliteStore;
use roomkit_tui::protocol::ModelInfo;
use roomkit_tui_engine::SessionConfig;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

use crate::storage::document::Document;
use crate::{
    agent::{CodeAgent, Inventory},
    config::Settings,
    storage::Storage,
};
#[cfg(test)]
pub(crate) use discovery::SUBSCRIPTION_SERVER;
pub(crate) use mcp::McpTools;
use model::{Agent, Authentication, PROJECT_SCHEMA, Project, Provider, Providers, TOOLS};
#[cfg(test)]
pub(crate) use provider::GEMINI_SERVER;
use slot::AgentSlot;
pub(crate) use worker::profile_id;

pub(crate) struct Profiles {
    pub(crate) executor_inventory: Option<crate::remote::pool::Inventory>,
    pub(crate) pool: Option<Arc<crate::remote::pool::Coordinator>>,
    pub(crate) policy: crate::permissions::Policy,
    pub(crate) plans: Arc<crate::plans::PlanView>,
    compact_tools: bool,
    require_tool_intent: bool,
    /// Rounds of tool calls one turn of an agent may run.
    max_tool_rounds: u32,
    /// No person answers approvals in this launch.
    unattended: bool,
    /// An unattended launch in Plan mode, which keeps the plan tools.
    planning: bool,
    /// What serves every agent's `ask_user`, in an attended launch.
    asking: Option<roomkit_ai::HumanInputToolHandler>,
    workspace: PathBuf,
    state_dir: PathBuf,
    pub(crate) diagnostics: Arc<crate::diagnostics::Diagnostics>,
    pub(crate) accounts: Arc<crate::accounts::Accounts>,
    pub(crate) resources: crate::agent::Resources,
    pub(crate) processes: Arc<crate::processes::Processes>,
    pub(crate) images: Arc<crate::images::Images>,
    skill_catalogue: tokio::sync::RwLock<skills::Catalogue>,
    pub(crate) mcp_runtime: Arc<mcp::Runtime>,
    project_memory: roomkit_tools_memory::MemoryTool,
    conversation_store: Option<SqliteStore>,
    /// The context capacity each connection's server reported for its
    /// models in its latest listing, by connection and model.
    reported: std::sync::Mutex<std::collections::HashMap<(String, String), u64>>,
    pub(crate) local_models: crate::local_models::LocalModels,
    state: Mutex<State>,
}

struct State {
    remote_worker: bool,
    instructions: Arc<crate::instructions::Instructions>,
    compactions: Arc<crate::compaction::Compactions>,
    /// The files the agents' tools change, which a slot settles as it closes.
    files: Arc<crate::files::Files>,
    policy: crate::permissions::Policy,
    mcp: Document<mcp::Servers>,
    pending_mcp: Option<mcp::Pending>,
    executor_form: Option<executors::Pending>,
    providers: Document<Providers>,
    project: Document<Project>,
    /// Agents shared by every project, beside the shared connections.
    global: Document<Project>,
    slots: BTreeMap<String, AgentSlot>,
    /// The requests of every agent's `ask_user`, which each slot declares to
    /// the engine as it registers.
    asking: Option<roomkit::human_input::HumanInputHandler>,
    tools: BTreeMap<String, Vec<AiTool>>,
    /// What each configured agent's requests carry before their messages.
    compositions: BTreeMap<String, crate::agent::Composition>,
    /// The agents a task can be handed to, kept as the agent files change.
    roster: Arc<crate::delegation::Roster>,
    /// What every agent's turns are told of what came before them.
    turn_ends: Arc<crate::turn_ends::TurnEnds>,
    /// What serves every agent's `delegate_task`, once the engine is up.
    delegation: Option<roomkit::tasks::DelegateHandler<SqliteStore>>,
    pending: Option<ui::Pending>,
    pending_delete: Option<deletion::Pending>,
    pending_gguf_removal: Option<ui::downloads::Removal>,
    setup_error: Option<String>,
    /// The model and the effort each agent runs with in the conversations
    /// this instance opened, as each conversation keeps them: by room and
    /// agent.
    running: BTreeMap<(String, String), crate::sessions::Running>,
    /// Why an agent could not be given the model and the effort a
    /// conversation keeps for it, by room and agent: its turns there do not
    /// start.
    unfitted: BTreeMap<(String, String), String>,
    /// The price the configuration states for one server and model.
    configured_price: Option<ConfiguredPrice>,
}

/// The tokens `profile`'s history compacts under: its context capacity, else
/// the kit's catalogue's window for its model, or the input past which the
/// catalogue's price for it rises when that is smaller (HITL a2daedfe,
/// db06dd3d). None without a window: the agent keeps its window of messages.
fn ceiling(profile: &Agent, kind: crate::providers::ProviderKind) -> Option<u64> {
    let listed = kind
        .models()
        .into_iter()
        .find(|entry| entry.id == profile.model);
    let window = profile
        .context_window
        .or_else(|| listed.as_ref().and_then(|entry| entry.context_window))
        .or_else(|| {
            (kind == crate::providers::ProviderKind::LlamaCpp)
                .then_some(crate::local_models::DEFAULT_CONTEXT.into())
        });
    let price = listed
        .and_then(|entry| entry.pricing)
        .and_then(|pricing| pricing.long_context_threshold_tokens);
    crate::compaction::ceiling(window, price)
}

/// `[provider.pricing]`: the list price of `model` on `base_url`.
struct ConfiguredPrice {
    kind: crate::providers::ProviderKind,
    base_url: String,
    model: String,
    pricing: roomkit_ai::ModelPricing,
}

/// The list price of `model` on `connection`: the kit's catalogue, else the
/// one the configuration states for that server and model. A ChatGPT
/// account, which a plan bills, reads its model's API list price too: what
/// the conversation would cost per token.
fn list_price(
    connection: &Provider,
    model: &str,
    configured: Option<&ConfiguredPrice>,
) -> Option<roomkit_ai::ModelPricing> {
    connection
        .kind
        .models()
        .into_iter()
        .find(|entry| entry.id == model)
        .and_then(|entry| entry.pricing)
        .or_else(|| {
            configured
                .filter(|price| {
                    price.kind == connection.kind
                        && price.base_url == connection.base_url
                        && price.model == model
                })
                .map(|price| price.pricing.clone())
        })
}

impl Profiles {
    /// No person answers approvals in this launch.
    pub(crate) fn unattended(&self) -> bool {
        self.unattended
    }

    pub async fn load(
        settings: &Settings,
        storage: &Storage,
        authentication: Authentication,
        resources: Option<crate::agent::Resources>,
        conversation_store: Option<SqliteStore>,
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
        } = startup::load(settings, storage, authentication).await?;
        let resources = match resources {
            Some(resources) => resources,
            None => startup::workspace_tools(settings, storage).await?,
        };
        let skill_catalogue =
            skills::Catalogue::load(settings.workspace.clone(), settings.state_dir.clone()).await?;
        let policy = crate::permissions::Policy::of_launch(settings.mode, settings.unattended);
        let instructions = resources.instructions.clone();
        let compactions = resources.compactions.clone();
        let files = resources.files.clone();
        let roster = resources.roster.clone();
        roster.enable_pool(settings.executor_pool.is_some());
        let turn_ends = resources.turn_ends.clone();
        // Nobody can answer a question in a launch nobody watches.
        let asking = (!settings.unattended)
            .then(crate::ask_user::handler)
            .transpose()
            .map_err(|error| error.to_string())?;
        let requests = asking.as_ref().map(|asking| asking.handler().clone());
        let pool = crate::remote::pool::Coordinator::for_launch(settings, &resources.tasks)
            .map_err(|e| e.to_string())?;
        let executor_inventory =
            crate::remote::pool::Inventory::for_worker(settings).map_err(|e| e.to_string())?;
        let mcp_runtime = Arc::new(mcp::Runtime::with_memory(settings.memory.server()));
        let project_memory = crate::project_memory::bind(settings, &mcp_runtime).await?;
        Ok(Arc::new(Self {
            project_memory,
            conversation_store,
            pool,
            local_models: Default::default(),
            executor_inventory,
            policy: policy.clone(),
            plans: crate::plans::PlanView::new((*resources.plans).clone()),
            compact_tools: settings.compact_tools,
            require_tool_intent: settings.require_tool_intent,
            max_tool_rounds: settings.max_tool_rounds,
            unattended: settings.unattended,
            planning: settings.unattended && settings.mode == crate::permissions::Mode::Plan,
            asking,
            skill_catalogue: tokio::sync::RwLock::new(skill_catalogue),
            images: crate::images::Images::new(storage.database.with_file_name("attachments")),
            diagnostics,
            workspace: settings.workspace.clone(),
            state_dir: settings.state_dir.clone(),
            accounts: crate::accounts::Accounts::new(&settings.state_dir.join("auth"))?,
            processes: crate::processes::Processes::new(resources.processes.clone()),
            resources,
            mcp_runtime,
            reported: std::sync::Mutex::default(),
            state: Mutex::new(State {
                remote_worker: settings.remote_worker.is_some() || settings.executor_pool.is_some(),
                instructions,
                compactions,
                files,
                policy,
                mcp,
                pending_mcp: None,
                executor_form: None,
                providers,
                project,
                global,
                slots: BTreeMap::new(),
                asking: requests,
                tools: BTreeMap::new(),
                compositions: BTreeMap::new(),
                roster,
                turn_ends,
                delegation: None,
                pending: None,
                pending_delete: None,
                pending_gguf_removal: None,
                setup_error: None,
                running: BTreeMap::new(),
                unfitted: BTreeMap::new(),
                configured_price: settings.pricing.clone().map(|pricing| ConfiguredPrice {
                    kind: settings.provider,
                    base_url: settings.base_url.clone(),
                    model: settings.model.clone(),
                    pricing,
                }),
            }),
        }))
    }

    /// Registers the agent the application opens with: the one `conversation`
    /// names, which the conversation it resumes designates, else the
    /// project's selection. A conversation's agent that no longer exists is
    /// not replaced: the view says why nothing can be sent.
    pub async fn initialize(
        &self,
        kit: &RoomKit<SqliteStore>,
        conversation: Option<ChannelId>,
    ) -> Result<ChannelId, String> {
        let mut state = self.state.lock().await;
        // A launch nobody watches ends with its turn, before a task it handed
        // over could end: no agent is served the delegation tools then.
        if !self.unattended {
            state.register_remote(kit)?;
            state.delegation = Some(crate::delegation::handler(kit));
            state.refresh_roster();
        }
        let id = match &conversation {
            Some(agent) => agent.to_string(),
            None => state.selected().unwrap_or_else(|| "code".into()),
        };
        if conversation.is_some() && state.agent(&id).is_none() {
            state.setup_error = Some(gone(&id));
        }
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
        state.register(kit, &channel, agent)?;
        if configured {
            state.compositions.insert(id.clone(), inventory.composition);
            state.tools.insert(id, inventory.tools);
        }
        Ok(channel)
    }

    pub(crate) fn active_pool(&self) -> Option<Arc<crate::remote::pool::Scheduler>> {
        self.pool.as_ref().and_then(|p| p.active())
    }

    pub async fn configure_view(&self, view: &mut SessionConfig) {
        self.state.lock().await.view(view);
    }

    /// The name of the connection agent `id` runs on.
    pub async fn provider_label(&self, id: &str) -> Option<String> {
        let state = self.state.lock().await;
        let agent = state.agent(id)?;
        Some(state.provider_label(&agent).to_owned())
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
}

/// What a view says of a conversation whose agent `id` was deleted.
fn gone(id: &str) -> String {
    format!("This conversation's agent, {id}, no longer exists: select an agent with /agents")
}

#[cfg(test)]
mod tests;
