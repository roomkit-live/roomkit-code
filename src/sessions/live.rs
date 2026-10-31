//! The live events a view receives: process and account panels, plan, hook
//! and update notices, and the sidebar, sent again whenever a source changes.

use std::sync::Arc;

use roomkit_tui::protocol::Event;
use roomkit_tui_engine::SessionConfig;
use tokio::sync::watch;

use super::Sessions;
use crate::{profiles::Profiles, services::Services};

/// Every source of a view's live events.
struct Sources {
    processes: watch::Receiver<Vec<Event>>,
    accounts: watch::Receiver<Option<Event>>,
    sidebar: watch::Receiver<()>,
    plans: watch::Receiver<()>,
    files: watch::Receiver<()>,
    updates: watch::Receiver<()>,
    hooks: watch::Receiver<()>,
}

pub(super) fn subscribe(
    sessions: &Sessions,
    current: SessionConfig,
) -> watch::Receiver<Vec<Event>> {
    let mut sources = Sources {
        processes: sessions.profiles.processes.subscribe(
            sessions.kit.clone(),
            sessions.channel.clone(),
            current.clone(),
        ),
        accounts: sessions.profiles.accounts.subscribe(),
        sidebar: sessions.services.sidebar_changes.subscribe(),
        plans: sessions.profiles.resources.plans.subscribe(),
        files: sessions.profiles.resources.files.subscribe(),
        updates: sessions.services.updates.changes.subscribe(),
        hooks: sessions.services.hooks.changes.subscribe(),
    };
    let (profiles, services) = (sessions.profiles.clone(), sessions.services.clone());
    let (tx, rx) = watch::channel(Vec::new());
    tokio::spawn(async move {
        loop {
            let (events, poll) = events(&profiles, &services, &current, &mut sources).await;
            if *tx.borrow() != events && tx.send(events).is_err() {
                break;
            }
            tokio::select! {
                () = tx.closed() => break,
                live = sources.changed(poll) => if !live { break; },
            }
        }
    });
    rx
}

/// What the view shows now, and whether MCP connections need polling.
async fn events(
    profiles: &Arc<Profiles>,
    services: &Services,
    current: &SessionConfig,
    sources: &mut Sources,
) -> (Vec<Event>, bool) {
    let mut events = sources.processes.borrow_and_update().clone();
    events.extend(profiles.plans.updates(current).await);
    events.extend(services.hooks.updates(current));
    events.extend(services.updates.events(current));
    if let Some(event) = sources.accounts.borrow_and_update().clone() {
        // Panels carry their conversation and opaque instance ID;
        // the UI ignores updates to a dismissed or different view.
        events.push(event);
    }
    let (mut sidebar, poll) = profiles.sidebar(current).await;
    if let Some(section) = services.hooks.sidebar(current) {
        sidebar.sections.push(section);
    }
    sidebar.sections.push(services.updates.sidebar());
    events.push(Event::SidebarChanged {
        conversation: current.room.as_str().into(),
        sidebar: Some(sidebar),
    });
    (events, poll)
}

impl Sources {
    /// Wait until a source changes, or two seconds while MCP connections need
    /// polling; false once a source is gone.
    async fn changed(&mut self, poll: bool) -> bool {
        tokio::select! {
            changed = self.processes.changed() => changed.is_ok(),
            changed = self.accounts.changed() => changed.is_ok(),
            changed = self.sidebar.changed() => changed.is_ok(),
            changed = self.plans.changed() => changed.is_ok(),
            changed = self.files.changed() => changed.is_ok(),
            changed = self.updates.changed() => changed.is_ok(),
            changed = self.hooks.changed() => changed.is_ok(),
            // MCP exposes connection liveness, but no disconnect stream. Only
            // configured connections are inspected; unchanged state never
            // redraws the TUI.
            () = tokio::time::sleep(std::time::Duration::from_secs(2)), if poll => true,
        }
    }
}
