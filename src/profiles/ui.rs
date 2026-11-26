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
    Hub(local::Destination, local::AfterDownload),
}

pub(super) fn choice(
    value: impl Into<String>,
    label: impl Into<String>,
    description: impl Into<String>,
) -> Choice {
    Choice::new(value, label).with_description(description)
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

mod action;
mod forms;
pub(in crate::profiles) mod local;
mod menus;
mod parse;

pub(super) use action::Action;
pub(super) use forms::next_id;
use forms::*;
pub(super) use menus::provider_choices;
use menus::*;
pub(super) use parse::value;
use parse::*;

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
                if let Some(args) = arguments.strip_prefix("gguf ") {
                    return self.local_action(kit, current, &name, args).await;
                }
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

    /// Does what `action` asks when it is one that takes the profiles' lock
    /// itself, or reaches a service before it does; none for the others.
    async fn unlocked(
        &self,
        kit: &RoomKit<SqliteStore>,
        terminal: &ChannelId,
        action: Action<'_>,
        current: &SessionConfig,
    ) -> Option<Result<CommandResult, String>> {
        let room = current.room.as_str();
        Some(match action {
            Action::Delete { command, id } => self.delete(command, id, current.clone()).await,
            Action::ConfirmDelete { command, token } => {
                self.confirm_delete(kit, terminal, command, token, current.clone())
                    .await
            }
            Action::ChatGpt(crate::accounts::Action::Use(account)) => {
                self.use_chatgpt(current, account).await
            }
            Action::ChatGpt(account_action) => {
                self.state.lock().await.pending_delete = None;
                self.accounts.handle(room, account_action).await
            }
            _ => return None,
        })
    }

    /// Does what `action` asks in `current`'s conversation.
    async fn invoke(
        &self,
        kit: &RoomKit<SqliteStore>,
        terminal: &ChannelId,
        action: Action<'_>,
        current: SessionConfig,
    ) -> Result<CommandResult, String> {
        if let Some(done) = self.unlocked(kit, terminal, action, &current).await {
            return done;
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
            Action::Delete { .. } | Action::ConfirmDelete { .. } | Action::ChatGpt(_) => {
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
            Target::Hub(dest, after) => {
                state.pending = None;
                drop(edit_lease);
                drop(state);
                return dest
                    .search(&next, value(values, "query").trim(), *after)
                    .await;
            }
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
                .map(|m| {
                    let on = Action::NewAgentOn {
                        provider: &provider_id,
                        model: &m.id,
                        typed: false,
                    };
                    choice(on.to_string(), m.name, m.id)
                })
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
