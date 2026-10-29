//! Application-owned engine lifecycle; the reusable TUI only receives a client.

use std::num::NonZeroUsize;

use roomkit::model::{ChannelId, RoomId};
use roomkit::{CreateRoomOptions, GetRoomOptions, RoomKit, RoomKitOptions};
use roomkit_cli_roomkit::{ConversationChannel, LocalSession, SessionConfig};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::{connection, protocol::ModelInfo};

use crate::{Result, agent, config::Settings, storage::Storage};

pub(crate) struct Application {
    pub kit: RoomKit<SqliteStore>,
    pub channel: ConversationChannel,
    pub view: SessionConfig,
    _storage: Storage,
}

impl Application {
    pub async fn open(settings: &Settings, api_key: String) -> Result<Self> {
        // Validate and assemble the agent before creating durable state.
        let agent = agent::create(settings, api_key).await?;
        let storage = Storage::open(&settings.state_dir, &settings.workspace)?;
        let store = SqliteStore::open(&storage.database).await?;
        let kit = RoomKit::new(RoomKitOptions::with_store(store));
        let setup = async {
            let agent_id = ChannelId::new("code")?;
            let channel = ConversationChannel::new(ChannelId::new("terminal")?);
            kit.register_channel(agent)?;
            kit.register_channel(channel.clone())?;
            let room = RoomId::new(&settings.session)?;
            match kit.get_room(&room, GetRoomOptions::default()).await {
                Ok(_) => {}
                Err(roomkit::KitError::Store(roomkit::StoreError::RoomNotFound(_))) => {
                    kit.create_room(CreateRoomOptions {
                        room_id: Some(room.clone()),
                        ..Default::default()
                    })
                    .await?;
                    for id in [channel.id(), &agent_id] {
                        kit.attach_channel(&room, id, Default::default()).await?;
                    }
                }
                Err(error) => return Err(error.into()),
            }
            let mut view = SessionConfig::new(room, agent_id, "local-user");
            view.title = format!("{} · {}", settings.workspace.display(), settings.session);
            view.model = Some(ModelInfo {
                provider: "openai-compatible".into(),
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
    let adapter = tokio::spawn(app.session().run(host));
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
