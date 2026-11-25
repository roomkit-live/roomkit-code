//! The owner's declaration and the workspace-local agent that fulfills it.

use super::*;
use crate::remote::pool::WorkerProfile;
use crate::remote::{AGENT, TOOLS as WORKER_TOOLS};

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
        Self::declare_remote(kit)
    }

    pub(super) fn undeclare_remote(kit: &RoomKit<SqliteStore>) -> Result<(), String> {
        let channel = ChannelId::new(AGENT).map_err(|e| e.to_string())?;
        kit.unregister_channel(&channel)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub(super) fn declare_remote(kit: &RoomKit<SqliteStore>) -> Result<(), String> {
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

/// A profile ID accepted by both the CLI and task placement.
pub(crate) fn profile_id(id: &str) -> Result<String, String> {
    model::valid_id(id)
        .then(|| id.to_owned())
        .ok_or_else(|| "Invalid worker profile ID".into())
}

/// All runtimes belong to the same transport channel. Each turn selects its
/// runtime from its task context, including follow-up turns in that room.
pub(super) struct WorkerAgents {
    pooled: bool,
    default: String,
    agents: BTreeMap<String, Arc<CodeAgent>>,
}

impl WorkerAgents {
    pub(super) fn select(
        &self,
        context: &roomkit::model::RoomContext,
    ) -> Result<(&str, Arc<CodeAgent>), String> {
        let requested = if self.pooled {
            crate::remote::pool::Requirements::parse(context.room.metadata.get("task_context"))?
                .profile
        } else {
            None
        };
        let id = requested.as_ref().unwrap_or(&self.default);
        self.agents
            .get_key_value(id)
            .map(|(id, a)| (id.as_str(), a.clone()))
            .ok_or_else(|| format!("Worker does not serve profile {id}"))
    }

    pub(super) fn runtimes(&self) -> Vec<Arc<CodeAgent>> {
        self.agents.values().cloned().collect()
    }
}

impl Profiles {
    pub(crate) async fn remote_authorized(&self) -> bool {
        self.state.lock().await.remote_worker
    }

    pub(crate) async fn worker(
        &self,
        requested: &[String],
    ) -> Result<(impl roomkit::Channel + Clone + use<>, Vec<WorkerProfile>), String> {
        let state = self.state.lock().await;
        let ids = if requested.is_empty() {
            vec![state.selected().ok_or(
                "Configure a model on the worker with --model and --base-url or a saved profile",
            )?]
        } else {
            requested.to_vec()
        };
        let mut agents = BTreeMap::new();
        let mut offered = Vec::new();
        for id in &ids {
            if agents.contains_key(id) {
                return Err(format!("Duplicate worker profile {id}"));
            }
            let (agent, info) = self.build_worker_profile(&state, id).await?;
            agents.insert(id.clone(), Arc::new(agent));
            offered.push(info);
        }
        let slot = AgentSlot::new(
            ChannelId::new(AGENT).map_err(|e| e.to_string())?,
            None,
            self.policy.clone(),
            state.instructions.clone(),
            state.files.clone(),
            state.compactions.clone(),
            state.roster.clone(),
        );
        Ok((
            slot.on_remote_executor(WorkerAgents {
                pooled: self.executor_inventory.is_some(),
                default: ids[0].clone(),
                agents,
            }),
            offered,
        ))
    }
    async fn build_worker_profile(
        &self,
        state: &State,
        id: &str,
    ) -> Result<(CodeAgent, WorkerProfile), String> {
        let mut profile = state
            .agent(id)
            .ok_or_else(|| format!("Unknown worker profile {id}"))?;
        let provider = state
            .providers
            .value
            .profiles
            .get(&profile.provider)
            .ok_or("The worker profile references an unknown connection")?;
        profile.tools.retain(|name| {
            WORKER_TOOLS.contains(&name.as_str())
                || (self.executor_inventory.is_some() && name == crate::names::CARGO_TEST)
        });
        profile.mcp.clear();
        profile.skills.clear();
        let (agent, inventory) = self
            .assemble_channel(&profile, provider, None, AGENT)
            .await
            .map_err(|e| format!("Worker profile {id}: {e}"))?;
        let mut tools: Vec<_> = inventory.tools.into_iter().map(|tool| tool.name).collect();
        // The engine adds this tool to every AI channel after assembly.
        tools.push("read_stored_result".into());
        tools.sort();
        tools.dedup();
        let info = WorkerProfile {
            id: profile.id,
            label: profile.label,
            model: profile.model,
            tools,
        };
        Ok((agent, info))
    }
}
