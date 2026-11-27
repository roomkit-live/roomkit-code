//! Human controls for app-owned background processes; never model tool calls.
use std::{collections::BTreeMap, sync::Arc, time::Duration};

use roomkit::model::{Access, ChannelId, RoomId};
use roomkit::{GetRoomOptions, ListBindingsOptions, RoomKit};
use roomkit_providers_sqlite::SqliteStore;
use roomkit_tools_process::{ObservedProcess, ProcessSession, ProcessState};
use roomkit_tui::protocol::{Event, SidebarContent, SidebarEntry, SidebarSection, SidebarTone};
use roomkit_tui_engine::SessionConfig;
use tokio::sync::watch;

mod panel;

use panel::Panel;

pub(crate) struct Processes {
    manager: ProcessSession,
    panel: watch::Sender<Option<Panel>>,
    sidebar: watch::Sender<(uuid::Uuid, SidebarSection)>,
}

struct Job {
    process: ObservedProcess,
    room: String,
    agent: String,
    writable: bool,
}

impl Processes {
    pub fn new(manager: ProcessSession) -> Arc<Self> {
        Arc::new(Self {
            manager,
            panel: watch::channel(None).0,
            sidebar: watch::channel((uuid::Uuid::nil(), sidebar(&[]))).0,
        })
    }

    pub fn sidebar(&self) -> SidebarSection {
        self.sidebar.borrow().1.clone()
    }

    /// The exit warning, then the background status, both counting the
    /// delegated tasks that run too. The terminal applies events in order but
    /// reads keys between bounded batches: the warning goes first, so a
    /// visible badge means Ctrl+D already asks for confirmation.
    async fn background(
        &self,
        current: &SessionConfig,
        running: usize,
        tasks: &crate::delegation::Tasks,
    ) -> [Event; 2] {
        let room = current.room.as_str();
        let delegated = tasks
            .family(room)
            .iter()
            .filter(|(_, t)| t.running())
            .count();
        [
            self.exit_warning(room, tasks.running()).await,
            Event::BackgroundChanged {
                conversation: room.into(),
                status: crate::background::status(running, delegated),
            },
        ]
    }

    /// Closing the application stops every running process and delegated
    /// task, whatever its owner or conversation; the terminal states it
    /// before detaching.
    async fn exit_warning(&self, conversation: &str, tasks: usize) -> Event {
        let running = self
            .manager
            .observe()
            .await
            .iter()
            .filter(|process| process.status.state == ProcessState::Running)
            .count();
        Event::ExitWarning {
            conversation: conversation.into(),
            text: crate::background::stopped(running, tasks),
        }
    }

    /// The UI owns observation lifetime. No periodic work while panels are
    /// closed. The exit warning and the background status count `tasks`' too.
    pub fn subscribe(
        self: &Arc<Self>,
        kit: RoomKit<SqliteStore>,
        viewer: ChannelId,
        current: SessionConfig,
        tasks: Arc<crate::delegation::Tasks>,
    ) -> watch::Receiver<Vec<Event>> {
        self.panel.send_replace(None);
        let observation = uuid::Uuid::new_v4();
        self.sidebar.send_replace((observation, sidebar(&[])));
        let (tx, rx) = watch::channel(Vec::new());
        let this = self.clone();
        let mut lifecycle = this.manager.subscribe();
        let mut panel_changes = this.panel.subscribe();
        let mut task_changes = tasks.subscribe();
        tokio::spawn(async move {
            loop {
                let panel = panel_changes
                    .borrow_and_update()
                    .clone()
                    .filter(|p| p.belongs_to(&current));
                let jobs = this.jobs(&kit, &viewer, &current).await;
                let running = running(&jobs);
                let mut events = Vec::from(this.background(&current, running, &tasks).await);
                if let Some(panel) = &panel {
                    events.push(Event::PanelUpdated {
                        conversation: current.room.as_str().into(),
                        panel: this.render(panel, &jobs).await,
                    });
                }
                let section_changed = this.publish(observation, sidebar(&jobs));
                let changed = *tx.borrow() != events || section_changed;
                if changed && tx.send(events).is_err() {
                    break;
                }
                tokio::select! {
                    biased;
                    () = tx.closed() => break,
                    changed = lifecycle.changed() => if changed.is_err() { break; },
                    changed = panel_changes.changed() => if changed.is_err() { break; },
                    changed = task_changes.changed() => if changed.is_err() { break; },
                    () = tokio::time::sleep(Duration::from_secs(1)), if panel.is_some() => {},
                }
            }
        });
        rx
    }

    /// Publishes `section` as observer `observation`'s, and whether it
    /// changed. A cancelled observer can finish its in-flight read after a
    /// view switch: it must not overwrite the new observer's section.
    fn publish(&self, observation: uuid::Uuid, section: SidebarSection) -> bool {
        self.sidebar.send_if_modified(|(current, previous)| {
            if *current != observation || *previous == section {
                return false;
            }
            *previous = section;
            true
        })
    }

    async fn jobs(
        &self,
        kit: &RoomKit<SqliteStore>,
        viewer: &ChannelId,
        current: &SessionConfig,
    ) -> Vec<Job> {
        let mut access = BTreeMap::new();
        let mut jobs = Vec::new();
        for process in self.manager.observe().await {
            // The RoomKit adapters, not the model, construct this owner tuple.
            let Some(Ok((room, agent))) = process
                .owner
                .as_deref()
                .map(serde_json::from_str::<(String, String)>)
            else {
                continue;
            };
            let permission = if let Some(permission) = access.get(&room) {
                *permission
            } else {
                let permission = room_access(kit, viewer, current, &room).await;
                access.insert(room.clone(), permission);
                permission
            };
            if let Some(writable) = permission {
                jobs.push(Job {
                    process,
                    room,
                    agent,
                    writable,
                });
            }
        }
        jobs
    }
}

async fn room_access(
    kit: &RoomKit<SqliteStore>,
    viewer: &ChannelId,
    current: &SessionConfig,
    id: &str,
) -> Option<bool> {
    let id = RoomId::new(id).ok()?;
    let room = kit
        .get_room(
            &id,
            GetRoomOptions {
                organization_id: current.organization.clone(),
            },
        )
        .await
        .ok()?;
    if room.organization_id != current.organization {
        return None;
    }
    let bindings = kit
        .list_bindings(
            &id,
            ListBindingsOptions {
                organization_id: current.organization.clone(),
            },
        )
        .await
        .ok()?;
    bindings
        .into_iter()
        .find_map(|b| (b.channel_id == *viewer).then_some(b.access))
        .and_then(|a| match a {
            Access::ReadWrite => Some(true),
            Access::ReadOnly => Some(false),
            _ => None,
        })
}

/// How many of `jobs` run.
fn running(jobs: &[Job]) -> usize {
    jobs.iter()
        .filter(|job| job.process.status.state == ProcessState::Running)
        .count()
}

// Only running jobs already authorized by jobs() reach the sidebar. Commands
// and owners are clipped before crossing the bounded UI protocol.
fn sidebar(jobs: &[Job]) -> SidebarSection {
    let mut entries = jobs
        .iter()
        .filter(|j| j.process.status.state == ProcessState::Running)
        .take(128)
        .map(|job| {
            SidebarEntry::new(format!(
                "● {}",
                job.process
                    .command
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(60)
                    .collect::<String>()
            ))
            .with_detail(format!(
                "Running · {}",
                job.agent.chars().take(64).collect::<String>()
            ))
            .with_tone(SidebarTone::Accent)
            .with_action(crate::profiles::sidebar::action(
                "processes",
                &job.process.status.process_id,
            ))
        })
        .collect::<Vec<_>>();
    if entries.is_empty() {
        entries.push(SidebarEntry::new("No running processes").with_tone(SidebarTone::Muted));
    }
    SidebarSection::new("Processes", SidebarContent::Entries { entries })
        .with_action(crate::profiles::sidebar::action("processes", ""))
}
