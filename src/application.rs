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

/// The channel of the application's views, the terminal's: what a row the
/// application writes for its views alone comes from.
pub(crate) const TERMINAL: &str = "terminal";

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
    pub async fn open(settings: &Settings, api_key: String) -> Result<Self> {
        Self::open_with_tools(settings, api_key, None).await
    }

    async fn open_with_tools(
        settings: &Settings,
        api_key: String,
        resources: Option<crate::agent::Resources>,
    ) -> Result<Self> {
        let storage = Storage::open(&settings.state_dir, &settings.workspace)?;
        let profiles = Profiles::load(settings, &storage, api_key, resources).await?;
        let services = Services::new(settings, &storage, &profiles)?;
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
        let kit = RoomKit::new(RoomKitOptions::with_store(store));
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
                abandon(&kit, &profiles).await;
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
        step("updates");
        self.services.updates.close().await;
        step("compactions");
        self.profiles.resources.compactions.close();
        step("shell");
        self.services.shell.close().await;
        step("accounts");
        self.profiles.accounts.close().await;
        step("mcp");
        self.profiles.mcp_runtime.close().await;
        // Closing cancels running shell calls: the agents' slots record what
        // they changed as they close, before the engine closes its store.
        step("engine");
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
    crate::delegation::install(kit, resources.tasks.clone(), resources.roster.clone())?;
    let agent_id = profiles.initialize(kit).await?;
    let channel = ConversationChannel::new(ChannelId::new(TERMINAL)?);
    kit.register_channel(channel.clone())?;
    profiles.resources.compactions.install(kit, channel.id());
    let gate = gate::Gate {
        policy: policy.clone(),
        channel: channel.clone(),
        hooks: services.hooks.clone(),
        plans: profiles.resources.plans.clone(),
        profiles: profiles.clone(),
        kit: kit.downgrade(),
    };
    gate::install(kit, gate)?;
    let view = first_view(
        settings, kit, profiles, services, policy, &channel, agent_id,
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
    view.mode = Some(policy.state(view.room.as_str()));
    profiles.attach(kit, &view, channel.id()).await?;
    profiles.configure_view(&mut view).await;
    let source = if settings.session.is_some() {
        "resume"
    } else {
        "startup"
    };
    services.hooks.note_source(view.room.as_str(), source);
    Ok(view)
}

/// Release what a failed setup opened.
async fn abandon(kit: &RoomKit<SqliteStore>, profiles: &Profiles) {
    let _ = kit.close().await;
    profiles.mcp_runtime.close().await;
    profiles.diagnostics.close().await;
    let _ = profiles.resources.close(false).await;
}

/// Events the terminal and the engine may queue for each other.
const QUEUE: NonZeroUsize = NonZeroUsize::new(256).unwrap();

pub(crate) async fn run(settings: Settings) -> Result<()> {
    let api_key = std::env::var("ROOMKIT_API_KEY").unwrap_or_default();
    let app = Application::open(&settings, api_key).await?;
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
    let resume = integrations::Resume::new(&settings);
    let integrations =
        integrations::Integration::start(&settings, app.profiles.diagnostics.clone(), &observer);
    let view = observer.clone();
    let terminal = tokio::task::spawn_blocking(move || {
        roomkit_tui::run_with_observer(client, renderers, settings.ui, view)
    })
    .await;
    integrations.freeze();
    let attached = adapter.await;
    let summary = summarize(&app, observer.latest(), &resume).await;
    summary.print_conversation();
    let closed = app.close().await;
    integrations.close().await;
    report(&app, &summary, closed.is_ok());
    terminal??;
    attached??;
    closed?;
    Ok(())
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
    let priced = match &view {
        Some(view) => {
            priced(
                app,
                view.conversation.as_str(),
                view.selected_agent.as_str(),
            )
            .await
        }
        None => None,
    };
    let shown = view.map(|view| {
        let command = view
            .mode
            .as_ref()
            .and_then(|mode| resume.command(view.conversation.as_str(), mode.selected.as_str()));
        (view.conversation.into_inner(), command)
    });
    let processes = &app.profiles.resources.processes;
    let mut summary = crate::exit_summary::collect(&app.kit, processes, shown, priced).await;
    if running {
        summary.turn_running();
    }
    summary
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
