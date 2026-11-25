//! Making a view show an agent: attached to its conversation, assembled
//! and designated.
use super::*;

impl Profiles {
    /// Only attach within a room whose terminal binding is already readable.
    /// True when the view's agent was attached by this call: what a caller
    /// whose next step fails gives to [`Self::saved_or_detached`].
    pub async fn attach(
        &self,
        kit: &RoomKit<SqliteStore>,
        view: &SessionConfig,
        terminal: &ChannelId,
    ) -> Result<bool, String> {
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
        if bindings.iter().any(|b| b.channel_id == view.agent) {
            return Ok(false);
        }
        kit.attach_channel(&view.room, &view.agent, Default::default())
            .await
            .map_err(|_| "Cannot attach the selected agent")?;
        Ok(true)
    }

    /// `saved`, the save an [`attach`](Self::attach) of `view`'s agent came
    /// before; when it failed, the agent `attached` then is detached again:
    /// a save that fails leaves the conversation as it was, not bound to an
    /// agent nothing selected.
    pub(super) async fn saved_or_detached<T>(
        &self,
        kit: &RoomKit<SqliteStore>,
        view: &SessionConfig,
        attached: bool,
        saved: Result<T, String>,
    ) -> Result<T, String> {
        if saved.is_err() && attached {
            let _ = kit
                .detach_channel(&view.room, &view.agent, Default::default())
                .await;
        }
        saved
    }

    /// Makes `view` show `agent`, the one its conversation designates,
    /// attached there and ready to answer. An agent that no longer exists
    /// or cannot be assembled is still the one shown: the view says why
    /// nothing can be sent, rather than send the conversation to another.
    pub async fn show(
        &self,
        kit: &RoomKit<SqliteStore>,
        view: &mut SessionConfig,
        terminal: &ChannelId,
        agent: ChannelId,
    ) -> Result<(), String> {
        let mut state = self.state.lock().await;
        let id = agent.to_string();
        view.agent = agent;
        state.ensure_slot(kit, &id)?;
        self.attach(kit, view, terminal).await?;
        if !state.tools.contains_key(&id) {
            let assembled = match state.agent(&id) {
                None => Err(gone(&id)),
                Some(profile) => match state.providers.value.profiles.get(&profile.provider) {
                    Some(provider) => self.assemble(&profile, provider).await,
                    None => Err("Configure the selected agent's provider with /agents".into()),
                },
            };
            match assembled {
                Ok((runtime, inventory)) => {
                    self.installed(kit, &mut state, &id, runtime, inventory)
                        .await?;
                }
                Err(error) => state.setup_error = Some(error),
            }
        }
        // With the model and the effort the conversation keeps for it.
        self.settle_in(kit, &mut state, view).await;
        Ok(())
    }

    /// Records `view`'s agent as the one its conversation designates. A
    /// task's room keeps the agent its task was handed to.
    pub(super) async fn designate(
        &self,
        kit: &RoomKit<SqliteStore>,
        view: &SessionConfig,
    ) -> Result<(), String> {
        let room = view.room.as_str();
        if self.resources.tasks.root_of(room) != room {
            return Ok(());
        }
        crate::sessions::designate(kit, &view.room, &view.agent)
            .await
            .map_err(|_| "Could not record this conversation's agent".into())
    }
}
