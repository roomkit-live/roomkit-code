//! Application-owned engine lifecycle; the reusable TUI only receives a client.

use std::num::NonZeroUsize;

use roomkit::ConversationStore;
use roomkit::model::ChannelId;
use roomkit::store::ListRoomsOptions;
use roomkit::{RoomKit, RoomKitOptions};
use roomkit_sqlite::{SqliteStore, SqliteStoreOptions};
use roomkit_tui::connection;
use roomkit_tui_engine::{ConversationChannel, LocalSession, SessionConfig};

use crate::{Result, config::Settings, profiles::Profiles, services::Services, storage::Storage};

mod gate;
pub(crate) mod headless;
mod integrations;
mod signals;
pub(crate) use signals::hangup_ignored;

use crate::names::TERMINAL;

pub(crate) struct Application {
    pub kit: RoomKit<SqliteStore>,
    pub channel: ConversationChannel,
    pub view: SessionConfig,
    pub policy: crate::permissions::Policy,
    pub profiles: std::sync::Arc<Profiles>,
    pub services: std::sync::Arc<Services>,
    _storage: Storage,
}

impl Application {
    /// Opens the application; `authentication` is that of the connection
    /// the launch options start a new project with.
    pub async fn open(
        settings: &Settings,
        authentication: impl Into<crate::profiles::model::Authentication>,
    ) -> Result<Self> {
        Self::open_with_tools(settings, authentication, None).await
    }

    async fn open_with_tools(
        settings: &Settings,
        authentication: impl Into<crate::profiles::model::Authentication>,
        resources: Option<crate::agent::Resources>,
    ) -> Result<Self> {
        let authentication = authentication.into();
        let storage = Storage::open(&settings.state_dir, &settings.workspace)?;
        let profiles = Profiles::load(settings, &storage, authentication, resources).await?;
        // From here the profiles hold processes, MCP connections and the
        // diagnostic log: a start that fails releases them and records where.
        let (services, kit) = match engine(settings, &storage, &profiles).await {
            Ok(opened) => opened,
            Err(error) => {
                abandon("engine", &error.to_string(), None, &profiles, None).await;
                return Err(error);
            }
        };
        let policy = profiles.policy.clone();
        match setup(settings, &kit, &profiles, &services, &policy).await {
            Ok((channel, view)) => Ok(Self {
                kit,
                channel,
                view,
                policy,
                profiles,
                services,
                _storage: storage,
            }),
            Err(error) => {
                abandon(
                    "setup",
                    &error.to_string(),
                    Some(&kit),
                    &profiles,
                    Some(&services),
                )
                .await;
                Err(error)
            }
        }
    }

    pub async fn close(&self) -> Result<()> {
        // Each step is recorded as it begins: a close that never ends shows
        // in the diagnostics where it stopped.
        let step = |step: &str| {
            let fields = serde_json::json!({"step": step});
            self.profiles
                .diagnostics
                .record("application_closing", fields, None);
        };
        self.profiles.resources.messages.close().await;
        step("updates");
        self.services.updates.close().await;
        step("compactions");
        self.profiles.resources.compactions.close().await;
        step("shell");
        self.services.shell.close().await;
        step("accounts");
        self.profiles.accounts.close().await;
        step("mcp");
        self.profiles.mcp_runtime.close().await;
        // The conversations' modes, kept as they change, once more before the
        // engine closes its store.
        if !self.profiles.unattended() {
            crate::sessions::keep_modes(&self.kit, &self.policy).await;
        }
        step("engine");
        // Closing cancels running shell calls: the agents' slots record what
        // they changed as they close, before the engine closes its store.
        let engine = self.kit.close().await;
        step("hooks");
        self.services.hooks.close().await;
        step("tools");
        let tools = self
            .profiles
            .resources
            .close(self.profiles.unattended())
            .await;
        self.profiles.diagnostics.close().await;
        engine?;
        if !tools {
            return Err("Workspace tool cleanup is still pending".into());
        }
        self.services.sessions.close();
        Ok(())
    }

    /// The router of the terminal's commands on this application.
    fn router(&self, workspace: &std::path::Path) -> crate::sessions::Sessions {
        crate::sessions::Sessions {
            kit: self.kit.clone(),
            workspace: workspace.to_path_buf(),
            channel: self.channel.id().clone(),
            policy: self.policy.clone(),
            profiles: self.profiles.clone(),
            services: self.services.clone(),
        }
    }

    pub fn session(&self) -> LocalSession<SqliteStore> {
        let compactions = self.profiles.resources.compactions.clone();
        let lifecycle = crate::compaction::lifecycle(self.services.hooks.lifecycle(), compactions);
        LocalSession::new(self.kit.clone(), self.channel.clone(), self.view.clone())
            .with_lifecycle(lifecycle)
    }
}

/// Install the application's hooks on the engine, then open the first
/// session and the view on it.
async fn setup(
    settings: &Settings,
    kit: &RoomKit<SqliteStore>,
    profiles: &std::sync::Arc<Profiles>,
    services: &Services,
    policy: &crate::permissions::Policy,
) -> Result<(ConversationChannel, SessionConfig)> {
    profiles.diagnostics.install(kit).await?;
    services.hooks.install(kit)?;
    profiles.resources.files.install(kit);
    let resources = &profiles.resources;
    resources
        .instructions
        .install(kit, profiles.diagnostics.clone());
    resources.turn_ends.install(kit)?;
    crate::delegation::install(kit, resources.tasks.clone(), resources.roster.clone())?;
    // A conversation that is resumed opens with the agent it designates, a
    // new one with the project's selection.
    let designated = match settings.session.as_deref().map(roomkit::model::RoomId::new) {
        Some(Ok(room)) => crate::sessions::designated(kit, &room).await?,
        _ => None,
    };
    if !settings.unattended {
        profiles.resources.messages.install(
            kit,
            settings.session_group.is_some(),
            profiles.pool.as_ref(),
        );
    }
    let agent_id = profiles.initialize(kit, designated).await?;
    let channel = ConversationChannel::new(ChannelId::new(TERMINAL)?);
    kit.register_channel(channel.clone())?;
    profiles.resources.compactions.install(kit, channel.id());
    let compactions = &profiles.resources.compactions;
    compactions.report_to(profiles.diagnostics.clone());
    let gate = gate::Gate {
        policy: policy.clone(),
        channel: channel.clone(),
        hooks: services.hooks.clone(),
        plans: profiles.resources.plans.clone(),
        profiles: profiles.clone(),
        kit: kit.downgrade(),
    };
    if settings.remote_worker.is_some() || profiles.pool.is_some() {
        gate::install_remote(kit, gate.clone())?;
    }
    gate::install(kit, gate)?;
    crate::ask_user::install(kit, channel.clone(), policy.clone())?;
    if !settings.unattended {
        crate::sessions::follow_modes(kit, policy);
        // Nobody reads an unattended launch past its answer: its agent is
        // told nothing a turn of its own would answer.
        profiles.resources.follow_job_ends(kit);
    }
    let view = first_view(
        settings, kit, profiles, services, policy, &channel, agent_id,
    )
    .await?;
    resources
        .messages
        .open_network(
            settings,
            kit,
            &view.room,
            services.sessions.clone(),
            &profiles.diagnostics,
        )
        .await?;
    Ok((channel, view))
}

/// The session the application opens with, resumed or new, and its view.
async fn first_view(
    settings: &Settings,
    kit: &RoomKit<SqliteStore>,
    profiles: &Profiles,
    services: &Services,
    policy: &crate::permissions::Policy,
    channel: &ConversationChannel,
    agent_id: ChannelId,
) -> Result<SessionConfig> {
    let room = crate::sessions::create(
        kit,
        &services.sessions,
        channel.id(),
        &agent_id,
        settings.session.as_deref(),
        settings.session.is_none(),
    )
    .await?;
    let mut view = SessionConfig::new(room, agent_id, "local-user");
    let name = crate::sessions::name(kit, &view.room).await;
    view.title = crate::sessions::title(&settings.workspace, &view.room, name.as_deref());
    view.commands = crate::sessions::commands();
    view.input_actions = vec![roomkit_tui::protocol::InputAction::new(
        '!',
        "shell",
        "Run a shell command in this project",
    )];
    view.input_actions.push(
        roomkit_tui::protocol::InputAction::new(
            '@',
            "message",
            "Choose a recipient or type its address and message",
        )
        .with_completion(),
    );
    view.cycle_command = Some(crate::delegation::panel::COMMAND.to_owned());
    // The engine hands a running turn only its own author's next messages,
    // and the application opens turns here too: a task's, the one a
    // background job's end or a task's result gives. One person writes in
    // these rooms, so every view hands what they write to the running turn.
    view.can_steer_any_turn = true;
    // A conversation someone watches opens in the mode it was left in,
    // unless the launch names one, and keeps the mode it is in. A launch
    // nobody watches keeps its own mode, and leaves the conversation's.
    if !settings.unattended {
        let kept = if settings.mode_given {
            None
        } else {
            crate::sessions::kept_mode(kit, &view.room).await
        };
        policy.open(view.room.as_str(), kept);
    }
    view.mode = Some(policy.state(view.room.as_str()));
    profiles.attach(kit, &view, channel.id()).await?;
    // Its agent runs with the model and the effort the conversation keeps.
    profiles.settle(kit, &mut view).await;
    let source = if settings.session.is_some() {
        "resume"
    } else {
        "startup"
    };
    services.hooks.note_source(view.room.as_str(), source);
    Ok(view)
}

/// The services and the engine on the project's store, opened and readable.
async fn engine(
    settings: &Settings,
    storage: &Storage,
    profiles: &std::sync::Arc<Profiles>,
) -> Result<(std::sync::Arc<Services>, RoomKit<SqliteStore>)> {
    let services = Services::new(settings, storage, profiles)?;
    let store = {
        let _initializing = storage.initialization_lease().await?;
        let store = SqliteStore::new(SqliteStoreOptions {
            path: storage.database.clone(),
        });
        // The store opens its file, creating or migrating its schema, at
        // its first operation: run one under the lease, which serializes
        // that across instances, and refuse a file it cannot open now.
        let first = ListRoomsOptions {
            limit: 1,
            ..ListRoomsOptions::default()
        };
        store.list_rooms(first).await?;
        store
    };
    let mut options = RoomKitOptions::with_store(store);
    if let Some(pool) = &profiles.pool {
        options.turn_backend = Some(roomkit::turns::TurnBackendHandle::new(pool.clone()));
    } else if let Some(group) = &settings.remote_worker {
        options.turn_backend = Some(crate::remote::handle(group)?);
    }
    Ok((services, RoomKit::new(options)))
}

/// Records that the start failed at `stage`, with its error's
/// classification, then releases what it opened: the engine and the
/// services when it got that far, and what the profiles hold.
async fn abandon(
    stage: &str,
    error: &str,
    kit: Option<&RoomKit<SqliteStore>>,
    profiles: &Profiles,
    services: Option<&Services>,
) {
    let fields = serde_json::json!({"stage": stage});
    profiles
        .diagnostics
        .record("application_start_failed", fields, Some(error));
    profiles.resources.messages.close().await;
    profiles.resources.compactions.close().await;
    if let Some(services) = services {
        services.shell.close().await;
    }
    profiles.accounts.close().await;
    if let Some(kit) = kit {
        let _ = kit.close().await;
    }
    if let Some(services) = services {
        services.hooks.close().await;
        services.sessions.close();
    }
    profiles.mcp_runtime.close().await;
    let _ = profiles.resources.close(false).await;
    profiles.diagnostics.close().await;
}

/// Events the terminal and the engine may queue for each other.
const QUEUE: NonZeroUsize = NonZeroUsize::new(256).unwrap();

/// How long a view whose host closed on a signal is given to return and
/// restore the terminal before the launch closes without it.
const VIEW_RETURNS: std::time::Duration = std::time::Duration::from_secs(2);

/// How long the SIGHUP of a terminal the view just failed on is waited for.
const HANGUP_FOLLOWS: std::time::Duration = std::time::Duration::from_millis(500);

pub(crate) async fn run(settings: Settings) -> Result<std::process::ExitCode> {
    // Listen first: a signal while the application opens still closes what
    // it opened.
    let signals = signals::listen()?;
    let app = Application::open(&settings, settings.initial_authentication()).await?;
    if let Some(signal) = signals.received() {
        app.close().await?;
        return Ok(signal.exit_code());
    }
    // Refuse an invalid attachment before entering alternate-screen mode.
    if let Err(error) = app.session().snapshot().await {
        let _ = app.close().await;
        return Err(error.into());
    }
    // From here the terminal's screen hides the default hook's message.
    crate::diagnostics::report_panics(app.profiles.diagnostics.clone());
    app.services.updates.start(settings.update_check);
    let (client, host) = connection(QUEUE);
    let adapter = tokio::spawn(
        app.session()
            .run_with_commands(host, app.router(&settings.workspace)),
    );
    let renderers = crate::tool_views::renderers(&settings.workspace);
    // The final observation outlives the view: it names the conversation to resume.
    let observer = roomkit_tui::SessionObserver::default();
    let resume = integrations::Resume::shown(&settings);
    let integrations = integrations::Integration::start(
        &settings,
        app.profiles.diagnostics.clone(),
        &observer,
        app.profiles.resources.tasks.clone(),
        app.profiles.resources.processes.clone(),
    );
    let view = observer.clone();
    let mut terminal = tokio::task::spawn_blocking(move || {
        roomkit_tui::run_with_observer(client, renderers, settings.ui, view)
    });
    let returned = until_left(&mut terminal, &adapter.abort_handle(), &signals).await;
    let left = matches!(&returned, Some(Ok(Ok(_))));
    integrations.freeze();
    let attached = adapter.await;
    let received = signals.received();
    // Only a view that returned as it should left a terminal to write the
    // report to: writing to one that failed, or that is gone, would panic
    // before anything is closed.
    let printed = left && received != Some(signals::Signal::Hangup);
    let summary = summarize(&app, observer.latest(), &resume).await;
    if printed {
        summary.print_conversation();
    }
    let closed = app.close().await;
    integrations.close().await;
    if printed {
        report(&app, &summary, closed.is_ok());
    }
    let Some(signal) = received else {
        // Without a signal the view returned.
        if let Some(returned) = returned {
            returned??;
        }
        attached??;
        closed?;
        return Ok(std::process::ExitCode::SUCCESS);
    };
    // The signal cut the view and its host: only the close can still fail.
    if returned.is_none() {
        // The view's thread may never end, and the runtime would wait for
        // it: a close that failed is recorded, then the process exits all
        // the same, rather than stay for ever.
        if let Err(error) = &closed {
            let error = error.to_string();
            let fields = serde_json::json!({});
            let diagnostics = &app.profiles.diagnostics;
            diagnostics.record("application_close_failed", fields, Some(&error));
        }
        signal.exit();
    }
    closed?;
    Ok(signal.exit_code())
}

/// What the view's thread returned.
type Returned =
    std::result::Result<std::io::Result<roomkit_tui::ExitReason>, tokio::task::JoinError>;

/// Waits for the view to return, which it does when the user leaves. On a
/// signal its host is closed, which makes a view on a live terminal return
/// and restore it: it is given a moment for that, whichever the signal, as
/// SIGHUP can come with the terminal still there. A view whose terminal is
/// gone may never return: none then, and the close does not depend on it.
async fn until_left(
    terminal: &mut tokio::task::JoinHandle<std::io::Result<roomkit_tui::ExitReason>>,
    host: &tokio::task::AbortHandle,
    signals: &signals::Signals,
) -> Option<Returned> {
    let returned = tokio::select! {
        returned = &mut *terminal => returned,
        _ = signals.wait() => {
            host.abort();
            return tokio::time::timeout(VIEW_RETURNS, terminal).await.ok();
        }
    };
    // A closed terminal can show first as the view failing to read or draw,
    // a moment before its SIGHUP is handled: the signal is given that
    // moment, so that the launch ends as hung up, not as a failed view.
    if !matches!(&returned, Ok(Ok(_))) {
        let _ = tokio::time::timeout(HANGUP_FOLLOWS, signals.wait()).await;
    }
    Some(returned)
}

/// The processes closing stopped, then a line when internal errors occurred.
fn report(app: &Application, summary: &crate::exit_summary::Summary, stopped: bool) {
    summary.print_processes(stopped);
    if let Some(notice) = app.profiles.diagnostics.panic_notice() {
        eprintln!("{notice}");
    }
}

/// Gather the exit report while the store and the processes are still open.
async fn summarize(
    app: &Application,
    view: Option<roomkit_tui::SessionObservation>,
    resume: &integrations::Resume,
) -> crate::exit_summary::Summary {
    let running = view
        .as_ref()
        .and_then(|view| view.run.as_ref())
        .is_some_and(|run| run.outcome.is_none());
    // A view can be in a task's room: the report is its conversation's.
    let tasks = &app.profiles.resources.tasks;
    let running = running || conversation_turn_runs(app, view.as_ref());
    let shown_room = view.as_ref().map(|view| {
        tasks.conversation_of(view.conversation.as_str(), view.selected_agent.as_str())
    });
    let priced = match &shown_room {
        Some((room, agent)) => priced(app, room, agent).await,
        None => None,
    };
    let shown = view.zip(shown_room).map(|(view, (room, _))| {
        let command = view
            .mode
            .as_ref()
            .and_then(|mode| resume.command(&room, mode.selected.as_str()));
        (room, command)
    });
    let processes = &app.profiles.resources.processes;
    let config = priced.clone();
    let mut summary = crate::exit_summary::collect(&app.kit, processes, shown, priced).await;
    if running {
        summary.turn_running();
    }
    if let Some(config) = config {
        add_delegated(app, &config, &mut summary).await;
    }
    summary
}

/// Whether the view is in a task's room while a turn runs in its conversation.
fn conversation_turn_runs(
    app: &Application,
    view: Option<&roomkit_tui::SessionObservation>,
) -> bool {
    let tasks = &app.profiles.resources.tasks;
    view.is_some_and(|view| {
        let root = tasks.root_of(view.conversation.as_str());
        root != view.conversation.as_str()
            && roomkit::model::RoomId::new(&root).is_ok_and(|room| app.channel.is_busy(&room))
    })
}

/// Adds to the report what the tasks of `config`'s conversation consumed.
async fn add_delegated(
    app: &Application,
    config: &SessionConfig,
    summary: &mut crate::exit_summary::Summary,
) {
    use crate::delegation::spending;
    let tasks = &app.profiles.resources.tasks;
    spending::read(&app.kit, &app.profiles, tasks, config).await;
    let listed: Vec<_> = tasks
        .of(config.room.as_str())
        .into_iter()
        .map(|(_, task)| task)
        .collect();
    if listed.is_empty() {
        return;
    }
    let leads = spending::leads(&app.kit, &app.profiles, config).await;
    summary.add_tasks(leads, &listed, config.agent.as_str());
}

/// The conversation shown at exit with its agent, priced as the profiles
/// price that agent.
async fn priced(app: &Application, conversation: &str, agent: &str) -> Option<SessionConfig> {
    let room = roomkit::model::RoomId::new(conversation).ok()?;
    let agent = ChannelId::new(agent).ok()?;
    let mut config = SessionConfig::new(room, agent, "local-user");
    app.profiles.configure_view(&mut config).await;
    Some(config)
}

#[cfg(test)]
mod tests;
