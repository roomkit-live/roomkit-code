//! Application-wide services that the terminal's commands and the engine's
//! hooks reach beside the profiles. Profiles owns connections, agents, MCP
//! servers and skills, and assembles agents; these run alongside.

use std::sync::Arc;

use crate::{config::Settings, profiles::Profiles, storage::Storage};

pub(crate) struct Services {
    /// Which conversations are open, in this process and in others.
    pub sessions: Arc<crate::storage::SessionLeases>,
    pub titles: Arc<crate::sessions::titles::Titles>,
    pub updates: Arc<crate::updates::Updates>,
    pub hooks: Arc<crate::hooks::Hooks>,
    /// The user's own `!` commands.
    pub shell: Arc<crate::shell::Shell>,
    /// Asks the sidebar to refresh after a change no other signal reports.
    pub sidebar_changes: tokio::sync::watch::Sender<()>,
}

impl Services {
    pub fn new(
        settings: &Settings,
        storage: &Storage,
        profiles: &Profiles,
    ) -> Result<Arc<Self>, String> {
        let state = storage.database.parent().ok_or("Missing state directory")?;
        let diagnostics = &profiles.diagnostics;
        let sidebar_changes = tokio::sync::watch::channel(()).0;
        Ok(Arc::new(Self {
            sessions: storage.sessions.clone(),
            titles: Arc::new(crate::sessions::titles::Titles::new(
                settings.auto_title,
                settings.workspace.clone(),
                sidebar_changes.clone(),
            )),
            updates: crate::updates::Updates::new(diagnostics.clone()),
            hooks: crate::hooks::Hooks::load(
                settings,
                state,
                profiles.policy.clone(),
                diagnostics.clone(),
            ),
            shell: crate::shell::Shell::new(
                profiles.resources.processes.clone(),
                diagnostics.clone(),
            ),
            sidebar_changes,
        }))
    }
}
