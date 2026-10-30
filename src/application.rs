//! Application-owned engine lifecycle; the reusable TUI only receives a client.

use std::num::NonZeroUsize;

use roomkit::model::ChannelId;
use roomkit::{RoomKit, RoomKitOptions};
use roomkit_cli_roomkit::{ConversationChannel, LocalSession, SessionConfig};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::{connection, protocol::ModelInfo};

use crate::{Result, agent, config::Settings, storage::Storage};

pub(crate) struct Application {
    pub kit: RoomKit<SqliteStore>,
    pub channel: ConversationChannel,
    pub view: SessionConfig,
    pub policy: crate::permissions::Policy,
    pub tools: Vec<roomkit_ai::ToolDefinition>,
    _storage: Storage,
}

impl Application {
    pub async fn open(settings: &Settings, api_key: String) -> Result<Self> {
        // Validate and assemble the agent before creating durable state.
        let (agent, tools) = agent::create(settings, api_key).await?;
        let storage = Storage::open(&settings.state_dir, &settings.workspace)?;
        let store = SqliteStore::open(&storage.database).await?;
        let kit = RoomKit::new(RoomKitOptions::with_store(store));
        let policy = crate::permissions::Policy::new(settings.mode);
        let setup = async {
            let agent_id = ChannelId::new("code")?;
            let channel = ConversationChannel::new(ChannelId::new("terminal")?);
            kit.register_channel(agent)?;
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
            view.context_capacity = settings.context_window;
            view.commands = crate::sessions::commands();
            view.mode = Some(policy.state(view.room.as_str()));
            view.model = Some(ModelInfo {
                provider: "openai".into(),
                model: settings.model.clone(),
                effort: None,
            });
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>((channel, view))
        }
        .await;
        match setup {
            Ok((channel, view)) => Ok(Self {
                kit,
                channel,
                view,
                policy,
                tools,
                _storage: storage,
            }),
            Err(error) => {
                let _ = kit.close().await;
                Err(error)
            }
        }
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
        let _ = app.kit.close().await;
        return Err(error.into());
    }
    let (client, host) = connection(NonZeroUsize::new(256).ok_or("Invalid queue capacity")?);
    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: settings.workspace.clone(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        tools: app.tools.clone(),
    };
    let adapter = tokio::spawn(app.session().run_with_commands(host, handler));
    let renderers = crate::tool_views::renderers(&settings.workspace);
    let terminal = tokio::task::spawn_blocking(move || {
        roomkit_tui::run_with_options(client, renderers, settings.ui)
    })
    .await;
    let attached = adapter.await;
    let closed = app.kit.close().await;
    terminal??;
    attached??;
    closed?;
    Ok(())
}

#[cfg(test)]
mod tests;
