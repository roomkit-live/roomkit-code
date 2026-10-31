//! Application information for the generic terminal sidebar. No credentials or
//! endpoint configuration cross this presentation boundary.
use super::*;
use roomkit_tui::protocol::{
    SidebarAction, SidebarContent, SidebarEntry, SidebarSection, SidebarSpec, SidebarTone,
};

pub(crate) fn action(command: &str, arguments: impl Into<String>) -> SidebarAction {
    SidebarAction::new(command, arguments)
}

impl Profiles {
    pub(crate) async fn sidebar(&self, current: &SessionConfig) -> (SidebarSpec, bool) {
        let state = self.state.lock().await;
        let agent = state.project.value.agents.get(current.agent.as_str());
        let mut agent_entries = Vec::new();
        if let Some(profile) = agent {
            let label = state
                .providers
                .value
                .profiles
                .get(&profile.provider)
                .map_or(profile.provider.as_str(), |provider| {
                    provider.label.as_str()
                });
            agent_entries.push(
                SidebarEntry::new(label.chars().take(64).collect::<String>())
                    .with_tone(SidebarTone::Muted)
                    .with_action(action("providers", "")),
            );
        }
        let mcp = mcp::sidebar(&state, current, &self.mcp_runtime).await;
        let configured = !state.mcp.value.servers.is_empty();
        (
            SidebarSpec::new(
                "Session",
                vec![
                    SidebarSection::new("Agent / model", SidebarContent::Agent)
                        .with_action(action("agents", "")),
                    SidebarSection::new(
                        "Provider",
                        SidebarContent::Entries {
                            entries: agent_entries,
                        },
                    ),
                    SidebarSection::new("Context", SidebarContent::Context),
                    self.plans.sidebar(current).await,
                    self.resources.files.sidebar(current.room.as_str()).await,
                    mcp,
                    self.processes.sidebar(),
                ],
            ),
            configured,
        )
    }
}
