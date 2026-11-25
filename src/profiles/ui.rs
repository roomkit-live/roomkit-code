use roomkit_tui::protocol::{
    Choice, Command, Event, FieldKind, FormField, FormSpec, FormValues, Request,
};
use roomkit_tui_engine::CommandResult;

use super::*;
use crate::providers::ProviderKind;
use catalog::Scope;

pub(super) struct Pending {
    room: String,
    form: FormSpec,
    target: Target,
}
#[derive(Clone)]
enum Target {
    Provider(Option<String>),
    /// A subscription's connection with a pasted key, new or `original`.
    Subscription {
        subscription: &'static super::subscriptions::Subscription,
        original: Option<String>,
    },
    Agent(Option<String>),
}

pub(super) fn choice(
    value: impl Into<String>,
    label: impl Into<String>,
    description: impl Into<String>,
) -> Choice {
    Choice::new(value, label).with_description(description)
}
fn effort_choices() -> Vec<Choice> {
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

pub(super) fn field(
    name: &str,
    label: &str,
    value: impl Into<String>,
    help: &str,
    required: bool,
    kind: FieldKind,
) -> FormField {
    let field = FormField::new(name, label, kind)
        .with_value(value)
        .with_help(help);
    if required { field.required() } else { field }
}
pub(super) fn choices(
    current: &SessionConfig,
    command: &str,
    title: &str,
    choices: Vec<Choice>,
) -> CommandResult {
    CommandResult::Events(vec![Event::Choices {
        conversation: current.room.as_str().into(),
        command: command.into(),
        title: title.into(),
        choices,
    }])
}

/// What `/providers`, `/agents`, `/effort` and `/model` are asked: the
/// command with the structured action a choice carries, or the words a
/// person types. Read once, here: what follows matches on it.
enum Action<'a> {
    /// `delete:` and `confirm-delete:`, which `deletion` reads.
    Delete {
        command: &'a str,
        arguments: &'a str,
    },
    /// An action on a ChatGPT account, which `accounts` reads.
    ChatGpt(&'a str),
    /// Saves the connection of a signed-in ChatGPT account.
    UseChatGpt(&'a str),
    EffortMenu,
    Effort(&'a str),
    /// `/model`, with a model's ID or nothing: an ID may hold colons, so it
    /// is the whole argument.
    Model(&'a str),
    Providers,
    OpenProvider(&'a str),
    AddProvider,
    EditProvider(&'a str),
    AddSubscription(&'a str),
    TestProvider(&'a str),
    /// A new agent on `provider`: on the listed `model`, or on one to type
    /// when it is empty.
    NewAgentOn {
        provider: &'a str,
        model: &'a str,
        typed: bool,
    },
    Agents,
    NewAgent(&'a str),
    AgentModels(&'a str),
    UseModel {
        agent: &'a str,
        model: &'a str,
    },
    OpenAgent(&'a str),
    AddAgent,
    EditAgent(&'a str),
    SelectAgent(&'a str),
}

impl<'a> Action<'a> {
    /// The action `arguments` name for `command`. `list` and `delete <id>`
    /// are the words a person types for the structured `` and `delete:<id>`.
    fn parse(command: &'a str, arguments: &'a str) -> Result<Self, String> {
        let arguments = arguments.trim();
        let listed = matches!(command, "agents" | "providers");
        if command == "model" {
            return Ok(Self::Model(arguments));
        }
        if listed
            && (arguments.starts_with("delete:")
                || arguments.starts_with("delete ")
                || arguments.starts_with("confirm-delete:"))
        {
            return Ok(Self::Delete { command, arguments });
        }
        if command == "providers" {
            if let Some(action) = arguments.strip_prefix("chatgpt:") {
                return Ok(Self::ChatGpt(action));
            }
            if let Some(account) = arguments.strip_prefix("chatgpt-use:") {
                return Ok(Self::UseChatGpt(
                    account.split(':').next().unwrap_or_default(),
                ));
            }
        }
        let mut parts = arguments.splitn(3, ':');
        let action = parts.next().unwrap_or_default();
        let id = parts.next().unwrap_or_default();
        let model = parts.next().unwrap_or_default();
        Ok(match (command, action) {
            ("effort", "") => Self::EffortMenu,
            ("effort", effort) => Self::Effort(effort),
            ("providers", "" | "list") => Self::Providers,
            ("providers", "open") => Self::OpenProvider(id),
            ("providers", "add") => Self::AddProvider,
            ("providers", "edit") => Self::EditProvider(id),
            ("providers", "subscription") => Self::AddSubscription(id),
            ("providers", "test") => Self::TestProvider(id),
            ("providers", "model" | "typed") => Self::NewAgentOn {
                provider: id,
                model,
                typed: action == "typed",
            },
            ("agents", "" | "list") => Self::Agents,
            ("agents", "new") => Self::NewAgent(id),
            ("agents", "models") => Self::AgentModels(id),
            ("agents", "use-model") => Self::UseModel { agent: id, model },
            ("agents", "open") => Self::OpenAgent(id),
            ("agents", "add") => Self::AddAgent,
            ("agents", "edit") => Self::EditAgent(id),
            ("agents", "select") => Self::SelectAgent(id),
            _ => return Err("Unknown profile action; reopen /providers or /agents".into()),
        })
    }

    /// Whether the action writes the profile files or what a conversation
    /// keeps: it then holds the cross-process edit lease. Menus, forms being
    /// opened and model discovery do not.
    fn mutates(&self) -> bool {
        match self {
            Self::SelectAgent(_) | Self::Effort(_) => true,
            Self::Model(id) => !id.is_empty(),
            _ => false,
        }
    }
}

/// What a connection's entry in `/providers` offers.
fn provider_menu(current: &SessionConfig, id: &str, provider: &Provider) -> CommandResult {
    let entries = if let Authentication::ChatGpt { account } = &provider.authentication {
        vec![
            choice(
                format!("chatgpt:account:{account}"),
                "Manage ChatGPT account",
                "Choose models, sign in or sign out",
            ),
            choice(
                format!("delete:{id}"),
                "Delete provider",
                "Remove this connection; account sign-out is separate",
            ),
        ]
    } else {
        vec![
            choice(
                format!("edit:{id}"),
                "Edit connection",
                if provider.subscription().is_some() {
                    "Name, API key and timeout"
                } else {
                    "URL, authentication and timeout"
                },
            ),
            choice(
                format!("test:{id}"),
                "Test connection and list models",
                "Read-only GET /models; no conversation is sent",
            ),
            choice(
                format!("delete:{id}"),
                "Delete provider",
                "Remove this shared connection if no agents use it",
            ),
        ]
    };
    choices(current, "providers", &provider.label, entries)
}

/// What an agent's entry in `/agents` offers.
fn agent_menu(current: &SessionConfig, id: &str, agent: &Agent) -> CommandResult {
    let entries = vec![
        choice(
            format!("select:{id}"),
            "Select agent",
            format!("{} · {}", agent.provider, agent.model),
        ),
        choice(
            format!("models:{id}"),
            "Change model",
            "Choose among the models its connection serves",
        ),
        choice(
            format!("edit:{id}"),
            "Edit agent",
            "Provider, model, effort, instructions and tools",
        ),
        choice(
            format!("delete:{id}"),
            "Delete agent",
            "Remove this profile; keep conversation history",
        ),
    ];
    choices(current, "agents", &agent.label, entries)
}

/// Opens the form of a new connection, or of connection `id`: its own form
/// for a subscription with a pasted key.
fn connection_form(
    state: &mut State,
    current: &SessionConfig,
    id: Option<&str>,
) -> Result<CommandResult, String> {
    let existing = match id {
        Some(id) => Some(
            state
                .providers
                .value
                .profiles
                .get(id)
                .ok_or("Unknown provider")?
                .clone(),
        ),
        None => None,
    };
    let Some(existing) = existing else {
        state.providers.value.check_room()?;
        let form = provider_form(None, state, uuid::Uuid::new_v4().to_string());
        return Ok(open_form(state, current, form, Target::Provider(None)));
    };
    if let Some(subscription) = existing.account_managed() {
        return Err(managed_elsewhere(subscription));
    }
    let form_id = uuid::Uuid::new_v4().to_string();
    let (form, target) = match existing.subscription() {
        Some(subscription) => (
            super::subscription_form::form(subscription, Some(&existing), form_id),
            Target::Subscription {
                subscription,
                original: Some(existing.id.clone()),
            },
        ),
        None => (
            provider_form(Some(&existing), state, form_id),
            Target::Provider(Some(existing.id.clone())),
        ),
    };
    Ok(open_form(state, current, form, target))
}

/// Opens the form of a new connection to subscription `id`, one whose
/// credential is a pasted key.
fn subscription_form(
    state: &mut State,
    current: &SessionConfig,
    id: &str,
) -> Result<CommandResult, String> {
    let subscription = super::subscriptions::find(id)
        .filter(|s| matches!(s.credential, super::subscriptions::Credential::Key { .. }))
        .ok_or("Unknown subscription")?;
    state.providers.value.check_room()?;
    let target = Target::Subscription {
        subscription,
        original: None,
    };
    let form = super::subscription_form::form(subscription, None, uuid::Uuid::new_v4().to_string());
    Ok(open_form(state, current, form, target))
}

/// Opens the form of a new agent on a connection's `model`, with the
/// capacity its server reported for it: a listed model, or one to type
/// when `typed`.
fn new_agent_form(
    state: &mut State,
    current: &SessionConfig,
    (provider, model, reported): (&str, &str, Option<u64>),
    typed: bool,
) -> Result<CommandResult, String> {
    if !state.providers.value.profiles.contains_key(provider)
        || model.is_empty() != typed
        || !(model.is_empty() || listable(model))
    {
        return Err("Invalid provider/model selection".into());
    }
    let on = Some((provider, model, reported));
    let form = agent_form(None, state, uuid::Uuid::new_v4().to_string(), on)?;
    Ok(open_form(state, current, form, Target::Agent(None)))
}

/// Opens an agent's form: a new one, or `existing`'s.
fn edit_agent_form(
    state: &mut State,
    current: &SessionConfig,
    existing: Option<&Agent>,
) -> Result<CommandResult, String> {
    let target = Target::Agent(existing.map(|agent| agent.id.clone()));
    let form = agent_form(existing, state, uuid::Uuid::new_v4().to_string(), None)?;
    Ok(open_form(state, current, form, target))
}

impl Profiles {
    pub async fn handle(
        &self,
        kit: &RoomKit<SqliteStore>,
        terminal: &ChannelId,
        request: Request,
        current: SessionConfig,
    ) -> Result<CommandResult, String> {
        match request.command {
            Command::Invoke { name, arguments } => {
                let action = Action::parse(&name, &arguments)?;
                self.invoke(kit, terminal, action, current).await
            }
            Command::SubmitForm { name, form, values }
                if name == "providers" && crate::accounts::callback_form_id(&form) =>
            {
                self.accounts
                    .submit_callback(current.room.as_str(), &form, &values)
                    .await
            }
            Command::SubmitForm { name, form, values } => {
                self.submit(kit, terminal, (&name, &form), &values, current)
                    .await
            }
            _ => Err("Unsupported profile request".into()),
        }
    }

    /// Does what `action` asks in `current`'s conversation.
    async fn invoke(
        &self,
        kit: &RoomKit<SqliteStore>,
        terminal: &ChannelId,
        action: Action<'_>,
        current: SessionConfig,
    ) -> Result<CommandResult, String> {
        // These take the profiles' lock themselves, or reach a service
        // before they do.
        match action {
            Action::Delete { command, arguments } => {
                // The words a person types for the structured action.
                let typed = arguments
                    .strip_prefix("delete ")
                    .map(|id| format!("delete:{}", id.trim()));
                let arguments = typed.as_deref().unwrap_or(arguments);
                return self
                    .delete(kit, terminal, command, arguments, current)
                    .await;
            }
            Action::ChatGpt(account_action) => {
                self.state.lock().await.pending_delete = None;
                return self
                    .accounts
                    .handle(current.room.as_str(), account_action)
                    .await;
            }
            Action::UseChatGpt(account) => return self.use_chatgpt(&current, account).await,
            _ => {}
        }
        let mut state = self.state.lock().await;
        state.pending_delete = None;
        let _edit_lease = action
            .mutates()
            .then(|| store::edit_lease(&self.state_dir))
            .transpose()?;
        state.pending = None;
        // Refresh shared connections only when opening a menu/form,
        // never just before saving an older form's revision.
        state.providers.reload()?;
        state.reload_agents()?;
        if state.providers.value.schema != 1 {
            return Err("Unsupported profile file version".into());
        }
        let provider = |state: &State, id: &str| {
            let found = state.providers.value.profiles.get(id);
            found.cloned().ok_or("Unknown provider")
        };
        match action {
            Action::Delete { .. } | Action::ChatGpt(_) | Action::UseChatGpt(_) => {
                Err("Unknown profile action; reopen /providers or /agents".into())
            }
            Action::Model(id) => self.model(kit, state, current, id).await,
            Action::EffortMenu => {
                state
                    .agent(current.agent.as_str())
                    .ok_or("Configure an agent with /agents first")?;
                let title = "Reasoning effort · supported levels depend on the model";
                Ok(choices(&current, "effort", title, effort_choices()))
            }
            Action::Effort(effort) => {
                self.change_current(kit, &mut state, current, |agent| {
                    agent.reasoning_effort = (effort != "default").then(|| effort.into());
                })
                .await
            }
            Action::Providers => Ok(provider_choices(&state, &current)),
            Action::OpenProvider(id) => Ok(provider_menu(&current, id, &provider(&state, id)?)),
            Action::AddProvider => connection_form(&mut state, &current, None),
            Action::EditProvider(id) => connection_form(&mut state, &current, Some(id)),
            Action::AddSubscription(id) => subscription_form(&mut state, &current, id),
            Action::TestProvider(id) => {
                let provider = provider(&state, id)?;
                // A listing reads a server: no profile command waits on it.
                drop(state);
                let title = "Connection successful · choose a model to create an agent";
                self.creation_choices(&current, &provider, title).await
            }
            Action::NewAgentOn {
                provider,
                model,
                typed,
            } => {
                let reported = self.reported(provider, model);
                let on = (provider, model, reported);
                new_agent_form(&mut state, &current, on, typed)
            }
            Action::Agents => Ok(agent_choices(&state, &current)),
            Action::NewAgent(id) => self.new_agent(state, current, id).await,
            Action::AgentModels(id) => self.agent_models(state, current, id).await,
            Action::UseModel { agent: id, model } => {
                let mut agent = state.agent(id).ok_or("Unknown agent")?;
                if !listable(model) {
                    return Err("Invalid agent/model selection".into());
                }
                self.switch(&mut agent, model);
                edit_agent_form(&mut state, &current, Some(&agent))
            }
            Action::OpenAgent(id) => {
                let agent = state.agent(id).ok_or("Unknown agent")?;
                Ok(agent_menu(&current, id, &agent))
            }
            Action::AddAgent => edit_agent_form(&mut state, &current, None),
            Action::EditAgent(id) => {
                let agent = state.agent(id).ok_or("Unknown agent")?;
                edit_agent_form(&mut state, &current, Some(&agent))
            }
            Action::SelectAgent(id) => self.select(kit, terminal, &mut state, current, id).await,
        }
    }

    /// `/agents select:<id>`: makes agent `id` the one `current`'s
    /// conversation designates and its view shows.
    async fn select(
        &self,
        kit: &RoomKit<SqliteStore>,
        terminal: &ChannelId,
        state: &mut State,
        current: SessionConfig,
        id: &str,
    ) -> Result<CommandResult, String> {
        let agent = state.agent(id).ok_or("Unknown agent")?.clone();
        let provider = state
            .providers
            .value
            .profiles
            .get(&agent.provider)
            .ok_or("Unknown provider")?;
        let (runtime, tools) = self.assemble(&agent, provider).await?;
        state.ensure_slot(kit, id)?;
        let mut next = current;
        next.agent = ChannelId::new(id).map_err(|_| "Invalid agent ID")?;
        let attached = self.attach(kit, &next, terminal).await?;
        let selected = state.select_agent(id);
        self.saved_or_detached(kit, &next, attached, selected)
            .await?;
        self.installed(kit, state, id, runtime, tools).await?;
        // Last: a selection that fails before leaves the
        // conversation with the agent its view still shows.
        self.designate(kit, &next).await?;
        self.settle_in(kit, state, &mut next).await;
        Ok(CommandResult::Switch(Box::new(next)))
    }

    /// Saves the form `form` of command `name`, submitted with `values`:
    /// the one this view opened, checked against what it asked.
    async fn submit(
        &self,
        kit: &RoomKit<SqliteStore>,
        terminal: &ChannelId,
        (name, form): (&str, &str),
        values: &FormValues,
        current: SessionConfig,
    ) -> Result<CommandResult, String> {
        let mut state = self.state.lock().await;
        state.pending_delete = None;
        let edit_lease = store::edit_lease(&self.state_dir)?;
        let pending = state
            .pending
            .as_ref()
            .ok_or("This form is no longer open")?;
        if pending.room != current.room.as_str()
            || pending.form.id != form
            || pending.form.command != name
        {
            return Err("This form does not belong to the current view".into());
        }
        let fields = &pending.form.fields;
        if values.len() != fields.len() || fields.iter().any(|f| values.get(&f.name).is_none()) {
            return Err("The form fields changed; cancel and reopen it".into());
        }
        let target = pending.target.clone();
        let mut next = current;
        let mut created = None;
        match &target {
            Target::Provider(original) => {
                let profile = parse_provider(values, original.as_deref(), &state)?;
                created = original.is_none().then(|| profile.clone());
                self.save_connection(kit, &mut state, &next, profile)
                    .await?;
            }
            Target::Subscription {
                subscription,
                original,
            } => {
                let original = original.as_deref();
                let profile =
                    super::subscription_form::parse(values, subscription, original, &state)?;
                created = original.is_none().then(|| profile.clone());
                self.save_connection(kit, &mut state, &next, profile)
                    .await?;
            }
            Target::Agent(original) => {
                let original = original.as_deref();
                self.save_agent_form(kit, terminal, &mut state, &mut next, (original, values))
                    .await?;
            }
        }
        state.pending = None;
        // A new connection the current agent does not use offers its
        // models; one it uses, recreated, refreshes its view.
        let current_uses = |p: &Provider| {
            state
                .agent(next.agent.as_str())
                .is_some_and(|a| a.provider == p.id)
        };
        if let Some(provider) = created.filter(|p| !current_uses(p)) {
            // A listing reads a server: no profile edit waits on it.
            drop(edit_lease);
            drop(state);
            return Ok(self.after_connecting(&next, &provider).await);
        }
        self.settle_in(kit, &mut state, &mut next).await;
        Ok(CommandResult::Switch(Box::new(next)))
    }

    /// Saves the agent an agent form describes, `original` when it edits
    /// one, and makes it the agent `next` shows.
    async fn save_agent_form(
        &self,
        kit: &RoomKit<SqliteStore>,
        terminal: &ChannelId,
        state: &mut State,
        next: &mut SessionConfig,
        (original, values): (Option<&str>, &FormValues),
    ) -> Result<(), String> {
        // A provider may have been deleted while this form was open.
        state.providers.reload()?;
        let before = original
            .and_then(|id| state.agent(id))
            .map(|agent| conversation::own(&agent));
        let (profile, scope) = parse_agent(values, original, state)?;
        let saved = conversation::own(&profile);
        let provider = state
            .providers
            .value
            .profiles
            .get(&profile.provider)
            .ok_or("Unknown provider")?;
        let (runtime, tools) = self.assemble(&profile, provider).await?;
        state.ensure_slot(kit, &profile.id)?;
        next.agent = ChannelId::new(&profile.id).map_err(|_| "Invalid agent ID")?;
        let attached = self.attach(kit, next, terminal).await?;
        let written = state
            .save_agent(profile, scope)
            .and_then(|()| state.select_agent(next.agent.as_str()));
        self.saved_or_detached(kit, next, attached, written).await?;
        self.installed(kit, state, next.agent.as_str(), runtime, tools)
            .await?;
        self.designate(kit, next).await?;
        // A model or an effort set in the form is also that of
        // the conversation it is saved from: the person set it
        // here. The agent's other conversations keep theirs.
        if before.as_ref() != Some(&saved) {
            self.adopt(kit, state, next, saved).await?;
        }
        Ok(())
    }

    /// `/providers chatgpt-use:<account>`: saves the connection of ChatGPT
    /// account `id`, then offers its models. The account's service lists
    /// them before the profiles are locked: a service that does not answer
    /// holds back neither the sidebar nor the other profile commands.
    async fn use_chatgpt(
        &self,
        current: &SessionConfig,
        id: &str,
    ) -> Result<CommandResult, String> {
        let account = self
            .accounts
            .auth
            .accounts()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or("Unknown ChatGPT account")?;
        let models = self
            .accounts
            .auth
            .models(id)
            .await
            .map_err(|e| e.to_string())?;
        if models.is_empty() {
            return Err("This ChatGPT account returned no available models".into());
        }
        let provider_id = format!("chatgpt-{id}");
        let chatgpt = &super::subscriptions::CHATGPT;
        let email = account.email.as_deref().unwrap_or("Account");
        let provider = Provider {
            subscription: None,
            kind: chatgpt.kind,
            id: provider_id.clone(),
            label: format!("ChatGPT · {}", email.chars().take(80).collect::<String>()),
            base_url: chatgpt.base_url.into(),
            authentication: Authentication::ChatGpt { account: id.into() },
            timeout_seconds: 600,
        };
        provider.validate()?;
        {
            let mut state = self.state.lock().await;
            state.pending_delete = None;
            state.pending = None;
            let _edit_lease = store::edit_lease(&self.state_dir)?;
            state.providers.reload()?;
            state.reload_agents()?;
            if state.providers.value.schema != 1 {
                return Err("Unsupported profile file version".into());
            }
            if !state.providers.value.profiles.contains_key(&provider_id) {
                state.providers.value.check_room()?;
            }
            let mut providers = state.providers.value.clone();
            providers.profiles.insert(provider_id.clone(), provider);
            state.providers.save(providers)?;
        }
        self.accounts.close().await;
        Ok(choices(
            current,
            "providers",
            "ChatGPT models · choose one to create an agent",
            models
                .into_iter()
                .map(|m| choice(format!("model:{provider_id}:{}", m.id), m.name, m.id))
                .collect(),
        ))
    }

    /// Saves a connection, reinstalling the agents that use it.
    async fn save_connection(
        &self,
        kit: &RoomKit<SqliteStore>,
        state: &mut State,
        current: &SessionConfig,
        profile: Provider,
    ) -> Result<(), String> {
        profile.check_credential_transport()?;
        let runtime = if let Some(agent) = state
            .agent(current.agent.as_str())
            .filter(|a| a.provider == profile.id)
        {
            Some(self.assemble(&agent, &profile).await?)
        } else {
            None
        };
        let connection = profile.id.clone();
        let mut providers = state.providers.value.clone();
        providers.profiles.insert(profile.id.clone(), profile);
        state.providers.save(providers)?;
        if let Some((agent, tools)) = runtime {
            self.installed(kit, state, current.agent.as_str(), agent, tools)
                .await?;
        }
        // The other agents on this connection hold its previous key and
        // endpoint.
        self.reinstall_others(kit, state, current.agent.as_str(), |agent| {
            agent.provider == connection
        })
        .await;
        Ok(())
    }
}

fn open_form(
    state: &mut State,
    current: &SessionConfig,
    form: FormSpec,
    target: Target,
) -> CommandResult {
    state.pending = Some(Pending {
        room: current.room.to_string(),
        form: form.clone(),
        target,
    });
    CommandResult::Events(vec![Event::Form {
        conversation: current.room.as_str().into(),
        form,
    }])
}

pub(super) fn provider_choices(state: &State, current: &SessionConfig) -> CommandResult {
    let mut entries = vec![choice(
        "add",
        "+ Add provider",
        "OpenAI-compatible or Anthropic connection",
    )];
    entries.extend(super::subscriptions::ALL.iter().map(|subscription| {
        let offer = &subscription.offer;
        choice(offer.action, offer.label, offer.description)
    }));
    entries.extend(state.providers.value.profiles.values().map(|p| {
        choice(
            format!("open:{}", p.id),
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
fn agent_choices(state: &State, current: &SessionConfig) -> CommandResult {
    let mut entries = vec![choice(
        "new",
        "+ Add agent",
        "Choose its connection and model, then its instructions and tools",
    )];
    for (agent, scope) in state.agents() {
        entries.push(choice(
            format!("open:{}", agent.id),
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

/// Whether a choice's `model` may preset a form: an ID a saved agent may
/// carry, whatever sent it.
fn listable(model: &str) -> bool {
    !model.trim().is_empty() && model.len() <= 256 && !model.chars().any(char::is_control)
}

pub(super) fn next_id(prefix: &str, occupied: impl Fn(&str) -> bool) -> String {
    (1..=129)
        .map(|n| format!("{prefix}-{n}"))
        .find(|id| !occupied(id))
        .unwrap_or_default()
}

fn provider_form(existing: Option<&Provider>, state: &State, id: String) -> FormSpec {
    let default = Provider {
        subscription: None,
        kind: ProviderKind::OpenAi,
        id: next_id("provider", |id| {
            state.providers.value.profiles.contains_key(id)
        }),
        label: "New provider".into(),
        base_url: String::new(),
        authentication: Authentication::None,
        timeout_seconds: 600,
    };
    let p = existing.unwrap_or(&default);
    let (auth, variable) = match &p.authentication {
        Authentication::ChatGpt { .. } => ("none", ""),
        Authentication::None => ("none", ""),
        Authentication::Stored { .. } => ("stored", ""),
        Authentication::Environment { variable } => ("environment", variable.as_str()),
    };
    FormSpec::new(
        id,
        "providers",
        "Provider connection",
        vec![
            field(
                "id",
                "ID",
                &p.id,
                "Stable ID; existing profiles cannot be renamed",
                true,
                FieldKind::Text,
            ),
            field(
                "label",
                "Name",
                &p.label,
                "Display name for this connection",
                true,
                FieldKind::Text,
            ),
            field(
                "kind",
                "Provider type",
                p.kind.value(),
                "The API protocol this connection uses",
                true,
                FieldKind::Choice {
                    choices: vec![
                        choice("openai", "OpenAI-compatible", "Chat Completions"),
                        choice("anthropic", "Anthropic", "Native Messages API"),
                    ],
                },
            ),
            field(
                "url",
                "API base URL",
                &p.base_url,
                "OpenAI: include /v1 when required. Anthropic: empty uses https://api.anthropic.com",
                false,
                FieldKind::Text,
            ),
            field(
                "auth",
                "Authentication",
                auth,
                "Choose no authentication, a local stored key, or an environment variable",
                true,
                FieldKind::Choice {
                    choices: vec![
                        choice("none", "No key", ""),
                        choice("stored", "Stored API key", ""),
                        choice("environment", "Environment variable", ""),
                    ],
                },
            ),
            field(
                "key",
                "API key",
                "",
                "Masked; empty keeps the existing stored key. Changing auth mode clears it",
                false,
                FieldKind::Secret,
            ),
            field(
                "variable",
                "Environment variable",
                variable,
                "Used only with environment authentication; e.g. MY_PROVIDER_API_KEY",
                false,
                FieldKind::Text,
            ),
            field(
                "timeout",
                "Timeout (seconds)",
                p.timeout_seconds.to_string(),
                "Maximum duration of one model request (1–86400)",
                true,
                FieldKind::Text,
            ),
        ],
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "Debt recorded when the limit was set: split it, never grow it"
)]
fn agent_form(
    existing: Option<&Agent>,
    state: &State,
    id: String,
    preset: Option<(&str, &str, Option<u64>)>,
) -> Result<FormSpec, String> {
    if state.providers.value.profiles.is_empty() {
        return Err("Add a connection with /providers first".into());
    }
    // Only a new agent's default ID avoids other projects' IDs.
    let elsewhere = if existing.is_none() {
        state.other_ids()
    } else {
        Vec::new()
    };
    let default = Agent {
        id: next_id("agent", |id| {
            state.scope(id).is_some() || elsewhere.iter().any(|used| used == id)
        }),
        label: "New agent".into(),
        provider: preset.map_or("", |p| p.0).into(),
        model: preset.map_or("", |p| p.1).into(),
        reasoning_effort: None,
        vision: false,
        skills: Vec::new(),
        mcp: BTreeMap::new(),
        context_window: preset.and_then(|p| p.2),
        max_output_tokens: None,
        instructions: String::new(),
        tools: TOOLS.iter().map(|s| (*s).into()).collect(),
    };
    let a = existing.unwrap_or(&default);
    Ok(FormSpec::new(
        id,
        "agents",
        "Agent profile · save and select",
        vec![
            field(
                "id",
                "ID",
                &a.id,
                "Stable author identity in history; existing profiles cannot be renamed",
                true,
                FieldKind::Text,
            ),
            field(
                "label",
                "Name",
                &a.label,
                "Name shown above this agent's messages",
                true,
                FieldKind::Text,
            ),
            field(
                "provider",
                "Provider",
                &a.provider,
                "Connection shared with other agents",
                true,
                FieldKind::Choice {
                    choices: state
                        .providers
                        .value
                        .profiles
                        .values()
                        .map(|p| choice(&p.id, &p.label, ""))
                        .collect(),
                },
            ),
            field(
                "model",
                "Model",
                &a.model,
                "Exact served model ID; /providers offers connection testing and discovery",
                true,
                FieldKind::Text,
            ),
            field(
                "context",
                "Context capacity",
                a.context_window.map_or_else(String::new, |n| n.to_string()),
                "Effective server capacity in tokens; empty means unknown",
                false,
                FieldKind::Text,
            ),
            field(
                "output",
                "Output budget (advanced)",
                a.max_output_tokens
                    .map_or_else(String::new, |n| n.to_string()),
                "Empty = Auto (includes reasoning); enter a token count only to set a fixed limit",
                false,
                FieldKind::Text,
            ),
            field(
                "effort",
                "Reasoning effort",
                a.reasoning_effort.as_deref().unwrap_or("default"),
                "Model-specific; default omits the parameter. Also available through /effort",
                true,
                FieldKind::Choice {
                    choices: effort_choices(),
                },
            ),
            field(
                "instructions",
                "Instructions",
                &a.instructions,
                "Additional instructions for this agent; Alt+Enter inserts a line break",
                false,
                FieldKind::Multiline,
            ),
            field(
                "tools",
                "Tools",
                a.tools.join(", "),
                &format!(
                    "Comma-separated: {}. Pool workers can also use cargo_test. Empty = no tools",
                    TOOLS.join(", ")
                ),
                false,
                FieldKind::Text,
            ),
            field(
                "vision",
                "Image input",
                if a.vision { "enabled" } else { "disabled" },
                "Enable only for models that accept images; Ctrl+V attaches a clipboard image",
                true,
                FieldKind::Choice {
                    choices: vec![
                        choice("disabled", "Disabled", "Text only"),
                        choice("enabled", "Enabled", "Model supports vision"),
                    ],
                },
            ),
            field(
                "scope",
                "Scope",
                existing
                    .and_then(|a| state.scope(&a.id))
                    .unwrap_or(Scope::Global)
                    .value(),
                "Global agents are available in every project; MCP servers and skills stay per project",
                true,
                FieldKind::Choice {
                    choices: vec![
                        choice("global", "Global", "Available in every project"),
                        choice("project", "This project", "Only in this project"),
                    ],
                },
            ),
        ],
    ))
}

fn managed_elsewhere(subscription: &super::subscriptions::Subscription) -> String {
    match subscription.credential {
        super::subscriptions::Credential::Account => format!(
            "Manage {name} connections through the {name} account menu",
            name = subscription.name
        ),
        super::subscriptions::Credential::Key { .. } => format!(
            "Edit {} connections from their own form in /providers",
            subscription.name
        ),
    }
}
pub(super) fn value<'a>(values: &'a FormValues, key: &str) -> &'a str {
    values.get(key).unwrap_or("")
}
fn identity(id: &str, original: Option<&str>, exists: bool) -> Result<(), String> {
    if original.is_some_and(|old| old != id) {
        return Err("Existing profile IDs cannot be renamed".into());
    }
    if original.is_none() && exists {
        return Err("This profile ID already exists".into());
    }
    Ok(())
}
fn parse_provider(
    values: &FormValues,
    original: Option<&str>,
    state: &State,
) -> Result<Provider, String> {
    let id = value(values, "id").trim();
    identity(
        id,
        original,
        state.providers.value.profiles.contains_key(id),
    )?;
    if let Some(subscription) = original
        .and_then(|id| state.providers.value.profiles.get(id))
        .and_then(Provider::subscription)
    {
        return Err(managed_elsewhere(subscription));
    }
    let authentication = match value(values, "auth") {
        "none" => Authentication::None,
        "environment" => Authentication::Environment {
            variable: value(values, "variable").trim().into(),
        },
        "stored" => {
            let entered = value(values, "key");
            let key = if entered.is_empty() {
                original
                    .and_then(|id| state.providers.value.profiles.get(id))
                    .and_then(|p| match &p.authentication {
                        Authentication::Stored { key } => Some(key.clone()),
                        _ => None,
                    })
                    .unwrap_or_default()
            } else {
                entered.into()
            };
            Authentication::Stored { key }
        }
        _ => return Err("Unknown authentication mode".into()),
    };
    let kind = match value(values, "kind") {
        "" => original
            .and_then(|id| state.providers.value.profiles.get(id))
            .map_or(ProviderKind::OpenAi, |p| p.kind),
        "openai" => ProviderKind::OpenAi,
        "anthropic" => ProviderKind::Anthropic,
        _ => return Err("Choose OpenAI-compatible or Anthropic".into()),
    };
    let url = value(values, "url").trim().trim_end_matches('/');
    let provider = Provider {
        subscription: None,
        kind,
        id: id.into(),
        label: value(values, "label").trim().into(),
        base_url: if url.is_empty() {
            kind.default_url()
        } else {
            url
        }
        .into(),
        authentication,
        timeout_seconds: value(values, "timeout")
            .trim()
            .parse()
            .map_err(|_| "Timeout must be an integer")?,
    };
    provider.validate()?;
    Ok(provider)
}
fn parse_agent(
    values: &FormValues,
    original: Option<&str>,
    state: &State,
) -> Result<(Agent, Scope), String> {
    let id = value(values, "id").trim();
    identity(id, original, state.scope(id).is_some())?;
    let scope = match value(values, "scope") {
        "global" => Scope::Global,
        "project" => Scope::Project,
        _ => return Err("Choose a global or project scope".into()),
    };
    if scope == Scope::Global && state.scope(id) != Some(Scope::Global) {
        state.check_global_id(id)?;
    }
    let capacity = value(values, "context").trim();
    let output = value(values, "output").trim();
    let profile =
        Agent {
            id: id.into(),
            label: value(values, "label").trim().into(),
            provider: value(values, "provider").into(),
            model: value(values, "model").trim().into(),
            vision: match value(values, "vision") {
                "" | "disabled" => false,
                "enabled" => true,
                _ => return Err("Choose enabled or disabled for image input".into()),
            },
            reasoning_effort: match value(values, "effort") {
                "" | "default" => None,
                effort => Some(effort.into()),
            },
            context_window: if capacity.is_empty() {
                None
            } else {
                Some(
                    capacity
                        .parse()
                        .map_err(|_| "Context capacity must be a positive integer")?,
                )
            },
            max_output_tokens: if output.is_empty() {
                None
            } else {
                Some(output.parse().map_err(
                    |_| "Max output tokens must be a positive number of tokens, or empty",
                )?)
            },
            instructions: value(values, "instructions").into(),
            mcp: original
                .and_then(|id| state.agent(id))
                .map(|agent| agent.mcp.clone())
                .unwrap_or_default(),
            skills: original
                .and_then(|id| state.agent(id))
                .map(|agent| agent.skills.clone())
                .unwrap_or_default(),
            tools: value(values, "tools")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
        };
    profile.validate()?;
    Ok((profile, scope))
}
