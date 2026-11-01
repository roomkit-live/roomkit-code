//! Application-owned engine lifecycle; the reusable TUI only receives a client.

use std::num::NonZeroUsize;

use roomkit::model::ChannelId;
use roomkit::{RoomKit, RoomKitOptions};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::connection;
use roomkit_tui_engine::{ConversationChannel, LocalSession, SessionConfig};

use crate::{Result, config::Settings, profiles::Profiles, services::Services, storage::Storage};

mod gate;
mod integrations;

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
            SqliteStore::open(&storage.database).await?
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
        self.services.updates.close().await;
        self.services.shell.close().await;
        self.profiles.accounts.close().await;
        self.profiles.mcp_runtime.close().await;
        let engine = self.kit.close().await;
        // Closing cancels running shell calls; record what they changed.
        self.profiles
            .resources
            .files
            .settle(crate::timeouts::SETTLE)
            .await;
        self.services.hooks.close().await;
        let tools = self.profiles.resources.close().await;
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
        LocalSession::new(self.kit.clone(), self.channel.clone(), self.view.clone())
            .with_lifecycle(self.services.hooks.lifecycle())
    }
}

/// Install the application's hooks on the engine, then open the first
/// session and the view on it.
async fn setup(
    settings: &Settings,
    kit: &RoomKit<SqliteStore>,
    profiles: &Profiles,
    services: &Services,
    policy: &crate::permissions::Policy,
) -> Result<(ConversationChannel, SessionConfig)> {
    profiles.diagnostics.install(kit).await?;
    services.hooks.install(kit)?;
    profiles.resources.files.install(kit);
    let agent_id = profiles.initialize(kit).await?;
    let channel = ConversationChannel::new(ChannelId::new("terminal")?);
    kit.register_channel(channel.clone())?;
    let gate = gate::Gate {
        policy: policy.clone(),
        channel: channel.clone(),
        hooks: services.hooks.clone(),
        plans: profiles.resources.plans.clone(),
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
    let _ = profiles.resources.close().await;
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
    let closed = app.close().await;
    integrations.close().await;
    report(&app, &summary, closed.is_ok());
    terminal??;
    attached??;
    closed?;
    Ok(())
}

/// The exit report, then a line when internal errors occurred.
fn report(app: &Application, summary: &crate::exit_summary::Summary, stopped: bool) {
    summary.print(stopped);
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
    let shown = view.map(|view| {
        let command = view
            .mode
            .as_ref()
            .and_then(|mode| resume.command(view.conversation.as_str(), mode.selected.as_str()));
        (view.conversation.into_inner(), command)
    });
    crate::exit_summary::collect(&app.kit, &app.profiles.resources.processes, shown).await
}

#[cfg(test)]
mod tests;
