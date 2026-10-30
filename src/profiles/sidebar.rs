//! Application information for the generic terminal sidebar. No credentials or
//! endpoint configuration cross this presentation boundary.
use super::*;
use roomkit_tui::protocol::{
    SidebarAction, SidebarContent, SidebarEntry, SidebarSection, SidebarSpec, SidebarTone,
};

pub(crate) fn action(command: &str, arguments: impl Into<String>) -> Option<SidebarAction> {
    Some(SidebarAction {
        command: command.into(),
        arguments: arguments.into(),
    })
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
            agent_entries.push(SidebarEntry {
                label: label.chars().take(64).collect(),
                detail: String::new(),
                tone: SidebarTone::Muted,
                action: action("providers", ""),
            });
        }
        let mcp = mcp::sidebar(&state, current, &self.mcp_runtime).await;
        let configured = !state.mcp.value.servers.is_empty();
        (
            SidebarSpec {
                title: "Session".into(),
                sections: vec![
                    SidebarSection {
                        title: "Agent / model".into(),
                        action: action("agents", ""),
                        content: SidebarContent::Agent,
                    },
                    SidebarSection {
                        title: "Provider".into(),
                        action: None,
                        content: SidebarContent::Entries(agent_entries),
                    },
                    SidebarSection {
                        title: "Context".into(),
                        action: None,
                        content: SidebarContent::Context,
                    },
                    mcp,
                    self.processes.sidebar(),
                ],
            },
            configured,
        )
    }
}
