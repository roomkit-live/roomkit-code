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
mod targets;
mod ui;

use roomkit::model::{Access, ChannelId};
use roomkit::{ListBindingsOptions, RoomKit};
use roomkit_ai::AiTool;
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::ModelInfo;
use roomkit_tui_engine::SessionConfig;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
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
    /// Rounds of tool calls one turn of an agent may run.
    max_tool_rounds: u32,
    /// No person answers approvals in this launch.
    unattended: bool,
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
    tools: BTreeMap<String, Vec<AiTool>>,
    /// What each configured agent's requests carry before their messages.
    compositions: BTreeMap<String, crate::agent::Composition>,
    /// The agents a task can be handed to, kept as the agent files change.
    roster: Arc<crate::delegation::Roster>,
    /// What serves every agent's `delegate_task`, once the engine is up.
    delegation: Option<roomkit::tasks::DelegateHandler<SqliteStore>>,
    pending: Option<ui::Pending>,
    pending_delete: Option<deletion::Pending>,
    setup_error: Option<String>,
    /// The price the configuration states for one server and model.
    configured_price: Option<ConfiguredPrice>,
}

/// What asks OpenAI to route an agent's requests to one prompt cache: a key,
/// a digest of the workspace and the agent which tells neither, as the body's
/// `prompt_cache_key` and as the `session-id` header, from which ChatGPT's
/// backend derives its cache affinity. Codex sends both, the same.
fn cache_routing(
    workspace: &Path,
    agent: &str,
) -> (roomkit::model::Metadata, BTreeMap<String, String>) {
    let digest = crate::digest::sha256_hex(format!("{}\0{agent}", workspace.display()));
    let key = format!("roomkit-code-{}", &digest[..32]);
    let mut body = roomkit::model::Metadata::new();
    body.insert("prompt_cache_key".into(), key.clone().into());
    (body, BTreeMap::from([("session-id".to_owned(), key)]))
}

/// The tokens `profile`'s history compacts under: its context capacity, else
/// the kit's catalogue's window for its model, or the input past which the
/// catalogue's price for it rises when that is smaller (HITL a2daedfe,
/// db06dd3d). None without a window: the agent keeps its window of messages.
fn ceiling(profile: &Agent) -> Option<u64> {
    let listed = <roomkit_openai::OpenAiAiProvider as roomkit_ai::AiProvider>::available_models()
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
    <roomkit_openai::OpenAiAiProvider as roomkit_ai::AiProvider>::available_models()
        .into_iter()
        .find(|entry| entry.id == model)
        .and_then(|entry| entry.pricing)
        .or_else(|| {
            configured
                .filter(|price| price.base_url == connection.base_url && price.model == model)
                .map(|price| price.pricing.clone())
        })
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
        let compactions = resources.compactions.clone();
        let files = resources.files.clone();
        let roster = resources.roster.clone();
        Ok(Arc::new(Self {
            policy: policy.clone(),
            plans: crate::plans::PlanView::new((*resources.plans).clone()),
            require_tool_intent: settings.require_tool_intent,
            max_tool_rounds: settings.max_tool_rounds,
            unattended: settings.unattended,
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
                tools: BTreeMap::new(),
                compositions: BTreeMap::new(),
                roster,
                delegation: None,
                pending: None,
                pending_delete: None,
                setup_error: None,
                configured_price: settings.pricing.clone().map(|pricing| ConfiguredPrice {
                    base_url: settings.base_url.clone(),
                    model: settings.model.clone(),
                    pricing,
                }),
            }),
        }))
    }

    pub async fn initialize(&self, kit: &RoomKit<SqliteStore>) -> Result<ChannelId, String> {
        let mut state = self.state.lock().await;
        // A launch nobody watches ends with its turn, before a task it handed
        // over could end: no agent is served the delegation tools then.
        if !self.unattended {
            state.delegation = Some(crate::delegation::handler(kit));
            state.refresh_roster();
        }
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
                // An agent's requests share their prompt's start: one key
                // routes them to one prompt cache. Without it, a turn's later
                // steps mostly missed it too (RMK-1481).
                let (extra_body, headers) = cache_routing(&self.workspace, &profile.id);
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
                    extra_body: Some(extra_body),
                    default_headers: Some(headers),
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
            // An effort is the person's choice, and every turn here has tools:
            // left unstated, the kit would drop it from each of them behind a
            // base URL. On OpenAI's own endpoint its catalogue still decides for
            // the models it tags.
            supports_reasoning_effort_with_tools: Some(true),
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
            max_tool_rounds: self.max_tool_rounds,
            unattended: self.unattended,
            delegation: !self.unattended,
            policy: self.policy.clone(),
            images: self.images.clone(),
            ceiling: ceiling(profile),
        };
        let created = crate::agent::create(
            &self.workspace,
            &self.resources,
            profile,
            config,
            extensions,
        )
        .await
        .map_err(
            |_| "Could not configure this agent; verify its provider, credential and workspace",
        )?;
        let compactions = &self.resources.compactions;
        compactions.set_bounded(&profile.id, ceiling(profile).is_some());
        Ok(created)
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
        );
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
            base_url: "http://localhost:8000/v1".into(),
            model: model.into(),
            pricing: roomkit_ai::ModelPricing::new(0.5, 1.5, verified).unwrap(),
        }
    }

    /// RMK-1481: one key per agent and workspace, the same at every launch,
    /// that names neither.
    #[test]
    fn an_agent_s_cache_key_is_its_own_and_stable() {
        let key = |workspace: &str, agent: &str| {
            let (body, headers) = cache_routing(Path::new(workspace), agent);
            let key = body["prompt_cache_key"].as_str().unwrap().to_owned();
            assert_eq!(
                headers,
                BTreeMap::from([("session-id".into(), key.clone())])
            );
            key
        };
        let code = key("/work/project", "code");
        assert_eq!(code, key("/work/project", "code"));
        assert_ne!(code, key("/work/project", "review"));
        assert_ne!(code, key("/work/other", "code"));
        assert!(
            code.starts_with("roomkit-code-") && code.len() == 45,
            "{code}"
        );
        assert!(!code.contains("work") && !code.contains("project"));
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
