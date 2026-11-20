//! The owner's declaration and the workspace-local agent that fulfills it.

use super::*;
use crate::remote::{AGENT, LABEL, TOOLS as WORKER_TOOLS};

impl State {
    pub(super) fn is_remote(&self, id: &str) -> bool {
        self.remote_worker && id == AGENT
    }

    pub(super) fn register_remote(&self, kit: &RoomKit<SqliteStore>) -> Result<(), String> {
        if !self.remote_worker {
            return Ok(());
        }
        if self.agent(AGENT).is_some() {
            return Err(format!(
                "--remote-worker conflicts with the local agent {AGENT}"
            ));
        }
        let channel = ChannelId::new(AGENT).map_err(|e| e.to_string())?;
        kit.register_channel(roomkit::ChannelDeclaration {
            steers_room_turns: true,
            serializes_room_turns: true,
            recent_events_window: crate::compaction::WINDOW,
            takes_owner_tools: false,
            ..roomkit::ChannelDeclaration::new(channel)
        })
        .map_err(|e| e.to_string())
    }
}

impl Profiles {
    /// The provider, instructions and workspace come from the worker machine.
    pub(crate) async fn worker(&self) -> Result<impl roomkit::Channel + Clone + use<>, String> {
        let state = self.state.lock().await;
        let mut profile = state.selected().and_then(|id| state.agent(&id)).ok_or(
            "Configure a model on the worker with --model and --base-url or a saved profile",
        )?;
        let provider = state
            .providers
            .value
            .profiles
            .get(&profile.provider)
            .ok_or("The worker profile references an unknown connection")?;
        profile.id = AGENT.into();
        profile.label = LABEL.into();
        profile
            .tools
            .retain(|name| WORKER_TOOLS.contains(&name.as_str()));
        profile.mcp.clear();
        profile.skills.clear();
        let (agent, _) = self.assemble(&profile, provider).await?;
        let slot = AgentSlot::new(
            ChannelId::new(AGENT).map_err(|e| e.to_string())?,
            Some(agent),
            self.policy.clone(),
            state.instructions.clone(),
            state.files.clone(),
            state.compactions.clone(),
            state.roster.clone(),
        );
        Ok(slot.on_remote_executor())
    }
}
