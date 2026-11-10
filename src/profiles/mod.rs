//! User connections and project agent composition, independent of terminal widgets.
mod catalog;
mod deletion;
mod discovery;
mod mcp;
pub(crate) mod model;
mod model_choice;
mod provider;
mod reasoning;
pub(crate) mod sidebar;
mod skills;
mod slot;
mod startup;
mod store;
mod subscription_form;
mod subscriptions;
mod targets;
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
#[cfg(test)]
pub(crate) use discovery::SUBSCRIPTION_SERVER;
pub(crate) use mcp::McpTools;
use model::{Agent, Authentication, PROJECT_SCHEMA, Project, Provider, Providers, TOOLS};
use slot::AgentSlot;
use store::Document;

pub(crate) struct Profiles {
    pub(crate) policy: crate::permissions::Policy,
    pub(crate) plans: Arc<crate::plans::PlanView>,
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
    pub(crate) mcp_runtime: mcp::Runtime,
    state: Mutex<State>,
}

struct State {
    instructions: Arc<crate::instructions::Instructions>,
    compactions: Arc<crate::compaction::Compactions>,
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
    setup_error: Option<String>,
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
        .or_else(|| listed.as_ref().and_then(|entry| entry.context_window));
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
        let compactions = resources.compactions.clone();
        let files = resources.files.clone();
        let roster = resources.roster.clone();
        let turn_ends = resources.turn_ends.clone();
        // Nobody can answer a question in a launch nobody watches.
        let asking = (!settings.unattended)
            .then(crate::ask_user::handler)
            .transpose()
            .map_err(|error| error.to_string())?;
        let requests = asking.as_ref().map(|asking| asking.handler().clone());
        Ok(Arc::new(Self {
            policy: policy.clone(),
            plans: crate::plans::PlanView::new((*resources.plans).clone()),
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
            mcp_runtime: mcp::Runtime::default(),
            state: Mutex::new(State {
                instructions,
                compactions,
                files,
                policy,
                mcp,
                pending_mcp: None,
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
                setup_error: None,
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

    async fn assemble(
        &self,
        profile: &Agent,
        provider: &Provider,
    ) -> Result<(CodeAgent, Inventory), String> {
        profile.validate()?;
        provider.validate()?;
        let provider_instance = self.generation_provider(profile, provider)?;
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
            max_tool_rounds: self.max_tool_rounds,
            unattended: self.unattended,
            planning: self.planning,
            delegation: !self.unattended,
            policy: self.policy.clone(),
            images: self.images.clone(),
            ceiling: ceiling(profile, provider.kind),
            human_input: self.asking.clone(),
        };
        let created = crate::agent::create(
            &self.workspace,
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
        compactions.set_bounded(&profile.id, ceiling(profile, provider.kind).is_some());
        Ok(created)
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

    /// Makes `view` show `agent`, the one its conversation designates,
    /// attached there and ready to answer. An agent that no longer exists
    /// or cannot be assembled is still the one shown: the view says why
    /// nothing can be sent, rather than send the conversation to another.
    pub async fn show(
        &self,
        kit: &RoomKit<SqliteStore>,
        view: &mut SessionConfig,
        terminal: &ChannelId,
        agent: ChannelId,
    ) -> Result<(), String> {
        let mut state = self.state.lock().await;
        let id = agent.to_string();
        view.agent = agent;
        state.ensure_slot(kit, &id)?;
        self.attach(kit, view, terminal).await?;
        if !state.tools.contains_key(&id) {
            let assembled = match state.agent(&id) {
                None => Err(gone(&id)),
                Some(profile) => match state.providers.value.profiles.get(&profile.provider) {
                    Some(provider) => self.assemble(&profile, provider).await,
                    None => Err("Configure the selected agent's provider with /agents".into()),
                },
            };
            match assembled {
                Ok((runtime, inventory)) => state.install(kit, &id, runtime, inventory)?,
                Err(error) => state.setup_error = Some(error),
            }
        }
        state.view(view);
        Ok(())
    }

    /// Records `view`'s agent as the one its conversation designates. A
    /// task's room keeps the agent its task was handed to.
    async fn designate(
        &self,
        kit: &RoomKit<SqliteStore>,
        view: &SessionConfig,
    ) -> Result<(), String> {
        let room = view.room.as_str();
        if self.resources.tasks.root_of(room) != room {
            return Ok(());
        }
        crate::sessions::designate(kit, &view.room, &view.agent)
            .await
            .map_err(|_| "Could not record this conversation's agent".into())
    }
}

/// What a view says of a conversation whose agent `id` was deleted.
fn gone(id: &str) -> String {
    format!("This conversation's agent, {id}, no longer exists: select an agent with /agents")
}

impl State {
    /// The name of the connection `agent` runs on, its identity without one.
    fn provider_label<'a>(&'a self, agent: &'a Agent) -> &'a str {
        self.providers
            .value
            .profiles
            .get(&agent.provider)
            .map_or(agent.provider.as_str(), |provider| provider.label.as_str())
    }

    fn view(&self, view: &mut SessionConfig) {
        let agent = self.agent(view.agent.as_str());
        let served = self.tools.contains_key(view.agent.as_str());
        view.can_attach_images = served && agent.as_ref().is_some_and(|a| a.vision);
        view.actor_names = self
            .agents()
            .into_iter()
            .map(|(a, _)| (a.id.as_str().into(), a.label))
            .collect();
        // What the application tells its views in a conversation, such as a
        // compaction's line, comes from the terminal's channel.
        view.actor_names.insert(
            crate::application::TERMINAL.into(),
            env!("CARGO_PKG_NAME").into(),
        );
        if let Some(profile) = agent {
            let kind = self
                .providers
                .value
                .profiles
                .get(&profile.provider)
                .map(|p| p.kind)
                .unwrap_or_default();
            // Match the engine's usage identity, including Responses over OAuth.
            view.model = Some(
                ModelInfo::new(kind.name(), profile.model.clone()).with_effort(
                    profile
                        .reasoning_effort
                        .clone()
                        .unwrap_or_else(|| "default".into()),
                ),
            );
            view.context_capacity = profile.context_window;
            view.pricing = self.pricing(&profile);
        } else {
            view.model = None;
            view.context_capacity = None;
            view.pricing = None;
        }
        view.submission_error = if self.tools.contains_key(view.agent.as_str()) {
            None
        } else {
            Some(self.setup_error.clone().unwrap_or_else(|| "Configure a connection with /providers, then create or select an agent with /agents".into()))
        };
    }

    fn pricing(&self, agent: &Agent) -> Option<roomkit_ai::ModelPricing> {
        let connection = self.providers.value.profiles.get(&agent.provider)?;
        list_price(connection, &agent.model, self.configured_price.as_ref())
    }

    /// Registers agent `id`'s slot, holding `agent`, with the engine, which
    /// serves it the delegation tools.
    fn register(
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

    fn ensure_slot(&mut self, kit: &RoomKit<SqliteStore>, id: &str) -> Result<(), String> {
        if !self.slots.contains_key(id) {
            let channel = ChannelId::new(id).map_err(|_| "Invalid agent ID")?;
            self.register(kit, &channel, None)?;
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
        self.put(kit, id, agent, inventory)?;
        self.setup_error = None;
        Ok(())
    }

    /// Puts `agent` in agent `id`'s slot, registering the slot first if needed.
    fn put(
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "Invalid fixtures fail the test")]
    use super::*;

    fn connection(authentication: Authentication) -> Provider {
        Provider {
            subscription: None,
            kind: crate::providers::ProviderKind::OpenAi,
            id: "local".into(),
            label: "Local".into(),
            base_url: "http://localhost:8000/v1".into(),
            authentication,
            timeout_seconds: 600,
        }
    }

    fn configured(model: &str) -> ConfiguredPrice {
        let verified = roomkit_ai::Date::new(2026, 11, 6).unwrap();
        ConfiguredPrice {
            kind: crate::providers::ProviderKind::OpenAi,
            base_url: "http://localhost:8000/v1".into(),
            model: model.into(),
            pricing: roomkit_ai::ModelPricing::new(0.5, 1.5, verified).unwrap(),
        }
    }

    /// RMK-1461: the kit's catalogue prices its models, a ChatGPT account's
    /// too, and the configuration a server's own.
    #[test]
    fn a_model_is_priced_by_the_catalogue_else_the_configuration() {
        let catalogue =
            <roomkit_openai::OpenAiAiProvider as roomkit_ai::AiProvider>::available_models()
                .into_iter()
                .find_map(|entry| Some((entry.id, entry.pricing?)))
                .unwrap();
        let key = connection(Authentication::None);
        let listed = list_price(&key, &catalogue.0, Some(&configured(&catalogue.0)));
        assert_eq!(listed, Some(catalogue.1.clone()));
        let local = configured("local-model");
        assert_eq!(
            list_price(&key, "local-model", Some(&local)),
            Some(local.pricing.clone())
        );
        assert_eq!(list_price(&key, "another-model", Some(&local)), None);
        let mut elsewhere = connection(Authentication::None);
        elsewhere.base_url = "http://another:8000/v1".into();
        assert_eq!(list_price(&elsewhere, "local-model", Some(&local)), None);
        let plan = connection(Authentication::ChatGpt {
            account: "account".into(),
        });
        assert_eq!(list_price(&plan, &catalogue.0, None), Some(catalogue.1));
    }
}
