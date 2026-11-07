//! The live events a view receives: process, task and account panels, plan,
//! hook and update notices, a compaction's notice and line, the sidebar and
//! the mode, sent again whenever a source changes.

use std::sync::Arc;
use std::time::Duration;

use roomkit_tui::protocol::Event;
use roomkit_tui_engine::SessionConfig;
use tokio::sync::watch;

use super::Sessions;
use crate::{profiles::Profiles, services::Services};

/// How often a view is polled while it needs it, unless a source asks sooner.
const POLL: Duration = Duration::from_secs(2);

/// What a view's live events are read for.
type View<'a> = (
    &'a roomkit::RoomKit<roomkit_sqlite::SqliteStore>,
    &'a Arc<Profiles>,
    &'a Services,
    &'a SessionConfig,
);

/// Every source of a view's live events.
struct Sources {
    processes: watch::Receiver<Vec<Event>>,
    accounts: watch::Receiver<Option<Event>>,
    sidebar: watch::Receiver<()>,
    plans: watch::Receiver<()>,
    files: watch::Receiver<()>,
    updates: watch::Receiver<()>,
    hooks: watch::Receiver<()>,
    /// A mode set by a mode tool, outside `/mode`.
    modes: watch::Receiver<()>,
    /// A conversation an agent's memory compacted.
    compactions: watch::Receiver<()>,
    /// A task a conversation's agent handed over started or ended.
    tasks: watch::Receiver<()>,
}

pub(super) fn subscribe(
    sessions: &Sessions,
    current: SessionConfig,
) -> watch::Receiver<Vec<Event>> {
    // A view attached shows no tasks panel, and the terminal drops one
    // without saying so: the panel is no longer followed.
    sessions.profiles.resources.tasks.panel.close();
    let mut sources = Sources {
        processes: sessions.profiles.processes.subscribe(
            sessions.kit.clone(),
            sessions.channel.clone(),
            current.clone(),
            sessions.profiles.resources.tasks.clone(),
        ),
        accounts: sessions.profiles.accounts.subscribe(),
        sidebar: sessions.services.sidebar_changes.subscribe(),
        plans: sessions.profiles.resources.plans.subscribe(),
        files: sessions.profiles.resources.files.subscribe(),
        updates: sessions.services.updates.changes.subscribe(),
        hooks: sessions.services.hooks.changes.subscribe(),
        modes: sessions.policy.changes.subscribe(),
        compactions: sessions.profiles.resources.compactions.subscribe(),
        tasks: sessions.profiles.resources.tasks.subscribe(),
    };
    let (profiles, services) = (sessions.profiles.clone(), sessions.services.clone());
    let (policy, kit) = (sessions.policy.clone(), sessions.kit.clone());
    let (tx, rx) = watch::channel(Vec::new());
    tokio::spawn(async move {
        loop {
            let view = (&kit, &profiles, &*services, &current);
            let (mut events, poll) = events(view, &mut sources).await;
            // The view always shows the mode the policy applies, whoever set it.
            events.push(Event::ModeChanged {
                conversation: current.room.as_str().into(),
                mode: policy.state(current.room.as_str()),
            });
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

/// What the view shows now, and how soon it needs polling: MCP connections,
/// a running compaction's or task's seconds, a task's conversation.
async fn events(
    (kit, profiles, services, current): View<'_>,
    sources: &mut Sources,
) -> (Vec<Event>, Option<Duration>) {
    let mut events = sources.processes.borrow_and_update().clone();
    events.extend(profiles.plans.updates(current).await);
    events.extend(services.hooks.updates(current));
    events.extend(services.updates.events(current));
    if let Some(event) = sources.accounts.borrow_and_update().clone() {
        // Panels carry their conversation and opaque instance ID;
        // the UI ignores updates to a dismissed or different view.
        events.push(event);
    }
    // In the status line, sent again while recent or running: a list of
    // events can be replaced before the view reads it. A compaction's line,
    // stored for the views attached later, is shown at once to this one.
    let room = current.room.as_str();
    let compactions = &profiles.resources.compactions;
    let tasks = &profiles.resources.tasks;
    let mut notices = compactions.notices(room);
    notices.extend(tasks.notices(room));
    let delegated = crate::delegation::panel::live(kit, tasks, current).await;
    events.extend(delegated.panel);
    events.extend(notices.into_iter().map(|text| Event::Notice {
        conversation: room.into(),
        text,
    }));
    events.extend(compactions.lines(room).map(|item| Event::Item {
        conversation: room.into(),
        item,
    }));
    let (mut sidebar, poll) = profiles.sidebar(current).await;
    // A running compaction's seconds are counted on screen.
    let poll = (poll || compactions.compacting(room)).then_some(POLL);
    let poll = poll.into_iter().chain(delegated.poll).min();
    sidebar.sections.push(delegated.section);
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
    /// Wait until a source changes, or `poll` while the view needs polling;
    /// false once a source is gone.
    async fn changed(&mut self, poll: Option<Duration>) -> bool {
        tokio::select! {
            changed = self.processes.changed() => changed.is_ok(),
            changed = self.accounts.changed() => changed.is_ok(),
            changed = self.sidebar.changed() => changed.is_ok(),
            changed = self.plans.changed() => changed.is_ok(),
            changed = self.files.changed() => changed.is_ok(),
            changed = self.updates.changed() => changed.is_ok(),
            changed = self.hooks.changed() => changed.is_ok(),
            changed = self.modes.changed() => changed.is_ok(),
            changed = self.compactions.changed() => changed.is_ok(),
            changed = self.tasks.changed() => changed.is_ok(),
            // MCP exposes connection liveness, but no disconnect stream. Only
            // configured connections are inspected; unchanged state never
            // redraws the TUI.
            () = tokio::time::sleep(poll.unwrap_or(POLL)), if poll.is_some() => true,
        }
    }
}
