//! Application-owned engine lifecycle; the reusable TUI only receives a client.

use std::num::NonZeroUsize;

use roomkit::model::ChannelId;
use roomkit::{RoomKit, RoomKitOptions};
use roomkit_cli_roomkit::{ConversationChannel, LocalSession, SessionConfig};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::connection;

use crate::{Result, config::Settings, profiles::Profiles, storage::Storage};

pub(crate) struct Application {
    pub kit: RoomKit<SqliteStore>,
    pub channel: ConversationChannel,
    pub view: SessionConfig,
    pub policy: crate::permissions::Policy,
    pub profiles: std::sync::Arc<Profiles>,
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
        let store = SqliteStore::open(&storage.database).await?;
        let kit = RoomKit::new(RoomKitOptions::with_store(store));
        let policy = crate::permissions::Policy::new(settings.mode);
        let setup = async {
            let agent_id = profiles.initialize(&kit).await?;
            let channel = ConversationChannel::new(ChannelId::new("terminal")?);
            kit.register_channel(channel.clone())?;
            let gate_policy = policy.clone();
            let gate_channel = channel.clone();
            kit.before_tool_use(
                roomkit::ToolHookSpec::new("code-permissions")
                    .with_timeout(std::time::Duration::from_secs(310)),
                move |event, _| {
                    let policy = gate_policy.clone();
                    let channel = gate_channel.clone();
                    async move { Ok(policy.gate(&channel, event).await) }
                },
            )?;
            let room = crate::sessions::create(
                &kit,
                channel.id(),
                &agent_id,
                settings.session.as_deref(),
                settings.session.is_none(),
            )
            .await?;
            let mut view = SessionConfig::new(room, agent_id, "local-user");
            view.title = crate::sessions::title(&settings.workspace, &view.room);
            view.commands = crate::sessions::commands();
            view.mode = Some(policy.state(view.room.as_str()));
            profiles.attach(&kit, &view, channel.id()).await?;
            profiles.configure_view(&mut view).await;
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
                _storage: storage,
            }),
            Err(error) => {
                let _ = kit.close().await;
                let _ = profiles.resources.close().await;
                Err(error)
            }
        }
    }

    pub async fn close(&self) -> Result<()> {
        let engine = self.kit.close().await;
        let tools = self.profiles.resources.close().await;
        engine?;
        if !tools {
            return Err("Workspace tool cleanup is still pending".into());
        }
        Ok(())
    }

    pub fn session(&self) -> LocalSession<SqliteStore> {
        LocalSession::new(self.kit.clone(), self.channel.clone(), self.view.clone())
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
    let (client, host) = connection(NonZeroUsize::new(256).ok_or("Invalid queue capacity")?);
    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: settings.workspace.clone(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
    };
    let adapter = tokio::spawn(app.session().run_with_commands(host, handler));
    let renderers = crate::tool_views::renderers(&settings.workspace);
    let terminal = tokio::task::spawn_blocking(move || {
        roomkit_tui::run_with_options(client, renderers, settings.ui)
    })
    .await;
    let attached = adapter.await;
    let closed = app.close().await;
    terminal??;
    attached??;
    closed?;
    Ok(())
}

#[cfg(test)]
mod tests;
