//! Application-owned engine lifecycle; the reusable TUI only receives a client.

use std::num::NonZeroUsize;

use roomkit::model::ChannelId;
use roomkit::{RoomKit, RoomKitOptions};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::connection;
use roomkit_tui_engine::{ConversationChannel, LocalSession, SessionConfig};

use crate::{Result, config::Settings, profiles::Profiles, services::Services, storage::Storage};

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
        let setup = async {
            profiles.diagnostics.install(&kit).await?;
            services.hooks.install(&kit)?;
            profiles.resources.files.install(&kit);
            let agent_id = profiles.initialize(&kit).await?;
            let channel = ConversationChannel::new(ChannelId::new("terminal")?);
            kit.register_channel(channel.clone())?;
            let gate_policy = policy.clone();
            let gate_channel = channel.clone();
            let gate_hooks = services.hooks.clone();
            kit.before_tool_use(
                roomkit::ToolHookSpec::new("code-permissions")
                    .with_timeout(crate::timeouts::PERMISSION_GATE),
                move |event, _| {
                    let policy = gate_policy.clone();
                    let channel = gate_channel.clone();
                    let hooks = gate_hooks.clone();
                    async move {
                        if policy.plan_blocks(&event) {
                            return Ok(policy.gate(&channel, event).await);
                        }
                        match hooks.before_tool(&event).await {
                            roomkit_hooks::Decision::Deny(reason) => {
                                Ok(roomkit::ToolUse::block(reason))
                            }
                            roomkit_hooks::Decision::Ask(reason) => {
                                Ok(policy.gate_with_review(&channel, event, Some(reason)).await)
                            }
                            roomkit_hooks::Decision::Continue => {
                                Ok(policy.gate(&channel, event).await)
                            }
                        }
                    }
                },
            )?;
            let room = crate::sessions::create(
                &kit,
                &services.sessions,
                channel.id(),
                &agent_id,
                settings.session.as_deref(),
                settings.session.is_none(),
            )
            .await?;
            let mut view = SessionConfig::new(room, agent_id, "local-user");
            view.title = crate::sessions::title(&settings.workspace, &view.room);
            view.commands = crate::sessions::commands();
            view.input_actions = vec![roomkit_tui::protocol::InputAction::new(
                '!',
                "shell",
                "Run a shell command in this project",
            )];
            view.mode = Some(policy.state(view.room.as_str()));
            profiles.attach(&kit, &view, channel.id()).await?;
            profiles.configure_view(&mut view).await;
            services.hooks.note_source(
                view.room.as_str(),
                if settings.session.is_some() {
                    "resume"
                } else {
                    "startup"
                },
            );
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>((channel, view))
        }
        .await;
        match setup {
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
                let _ = kit.close().await;
                profiles.mcp_runtime.close().await;
                profiles.diagnostics.close().await;
                let _ = profiles.resources.close().await;
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
            .settle(std::time::Duration::from_secs(10))
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

    pub fn session(&self) -> LocalSession<SqliteStore> {
        LocalSession::new(self.kit.clone(), self.channel.clone(), self.view.clone())
            .with_lifecycle(self.services.hooks.lifecycle())
    }
}

pub(crate) async fn run(settings: Settings) -> Result<()> {
    let api_key = std::env::var("ROOMKIT_API_KEY").unwrap_or_default();
    let app = Application::open(&settings, api_key).await?;
    // Refuse an invalid attachment before entering alternate-screen mode.
    if let Err(error) = app.session().snapshot().await {
        let _ = app.close().await;
        return Err(error.into());
    }
    app.services.updates.start(settings.update_check);
    let (client, host) = connection(NonZeroUsize::new(256).ok_or("Invalid queue capacity")?);
    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: settings.workspace.clone(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
        services: app.services.clone(),
    };
    let adapter = tokio::spawn(app.session().run_with_commands(host, handler));
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
    summary.print(closed.is_ok());
    terminal??;
    attached??;
    closed?;
    Ok(())
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
