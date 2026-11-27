//! One background title request at a time per owned conversation.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use roomkit::RoomKit;
use roomkit::model::{EventContent, RoomId};
use roomkit_providers_sqlite::SqliteStore;
use roomkit_tui::protocol::{Event, OperationId};
use roomkit_tui_engine::{SessionConfig, TurnOutcome};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use super::names;
use crate::{profiles::Profiles, storage::SessionLeases, sync::lock};

mod generate;

type Kit = RoomKit<SqliteStore>;
struct Proposal {
    provider: crate::providers::CodeProvider,
    max_tokens: u32,
    text: String,
}

pub(crate) struct Titles {
    enabled: bool,
    workspace: PathBuf,
    /// While generating, keep the latest eligible prompt in case the first has no topic.
    queued: Mutex<HashMap<RoomId, Option<Proposal>>>,
    known: Mutex<HashMap<RoomId, String>>,
    write: tokio::sync::Mutex<()>,
    changes: watch::Sender<()>,
    stop: CancellationToken,
    tasks: TaskTracker,
}

impl Titles {
    pub fn new(enabled: bool, workspace: PathBuf, changes: watch::Sender<()>) -> Self {
        Self {
            enabled,
            workspace,
            queued: Mutex::default(),
            known: Mutex::default(),
            write: tokio::sync::Mutex::default(),
            changes,
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
        }
    }

    pub fn event(&self, room: &RoomId) -> Option<Event> {
        lock(&self.known).get(room).map(|name| Event::TitleChanged {
            conversation: room.as_str().into(),
            title: super::title(&self.workspace, room, Some(name)),
        })
    }

    /// Only the owner may name a conversation. Manual and automatic writes share
    /// this short critical section; model generation never holds the write lock.
    pub(super) async fn save(
        &self,
        kit: &Kit,
        room: &RoomId,
        name: &str,
        automatic: bool,
    ) -> Result<(), String> {
        let _guard = self.write.lock().await;
        if automatic && self.already_named(kit, room).await? {
            return Ok(());
        }
        names::set(kit, room, name).await?;
        self.remember(room, name.to_owned());
        Ok(())
    }

    fn remember(&self, room: &RoomId, name: String) {
        lock(&self.known).insert(room.clone(), name);
        self.changes.send_replace(());
    }

    async fn already_named(&self, kit: &Kit, room: &RoomId) -> Result<bool, String> {
        if lock(&self.known).contains_key(room) {
            return Ok(true);
        }
        if let Some(name) = names::read(kit, room)
            .await
            .map_err(|_| "Could not read conversation title")?
        {
            lock(&self.known).entry(room.clone()).or_insert(name);
            return Ok(true);
        }
        Ok(false)
    }

    fn schedule(
        self: &Arc<Self>,
        kit: Kit,
        profiles: Arc<Profiles>,
        room: RoomId,
        proposal: Proposal,
    ) {
        {
            use std::collections::hash_map::Entry;
            match lock(&self.queued).entry(room.clone()) {
                Entry::Occupied(mut entry) => {
                    entry.insert(Some(proposal));
                    return;
                }
                Entry::Vacant(entry) => {
                    entry.insert(None);
                }
            }
        }
        let titles = self.clone();
        self.tasks.spawn(async move {
            let mut next = proposal;
            loop {
                tokio::select! {
                    biased;
                    () = titles.stop.cancelled() => break,
                    result = titles.generate(&kit, &room, next) => {
                        if let Err(error) = result {
                            record_failure(&profiles, &room, &error);
                        }
                    }
                }
                let mut queued = lock(&titles.queued);
                if let Some(more) = queued.get_mut(&room).and_then(Option::take) {
                    next = more;
                } else {
                    queued.remove(&room);
                    break;
                }
            }
        });
    }

    async fn generate(&self, kit: &Kit, room: &RoomId, topic: Proposal) -> Result<(), String> {
        if self.already_named(kit, room).await? {
            return Ok(());
        }
        if let Some(title) = tokio::time::timeout(
            Duration::from_secs(45),
            generate::title(topic.provider, topic.max_tokens, topic.text),
        )
        .await
        .map_err(|_| "Title request timed out")??
        {
            self.save(kit, room, &title, true).await?;
        }
        Ok(())
    }

    pub async fn close(&self) {
        self.stop.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
}

/// Match accepted user inputs with the outcome of their own operation. Synthetic
/// answers and private worker conversations never supply a title prompt.
pub(crate) struct TitleJobs {
    titles: Arc<Titles>,
    kit: Kit,
    profiles: Arc<Profiles>,
    leases: Arc<SessionLeases>,
    inputs: Mutex<HashMap<(RoomId, OperationId), String>>,
}

impl TitleJobs {
    pub fn new(
        titles: Arc<Titles>,
        kit: Kit,
        profiles: Arc<Profiles>,
        leases: Arc<SessionLeases>,
    ) -> Self {
        Self {
            titles,
            kit,
            profiles,
            leases,
            inputs: Mutex::default(),
        }
    }

    pub fn admitted(&self, view: &SessionConfig, operation: &OperationId, content: &EventContent) {
        if !self.titles.enabled
            || !self.leases.owns(view.room.as_str())
            || view.group.is_some()
            || lock(&self.titles.known).contains_key(&view.room)
        {
            return;
        }
        if let Some(text) = generate::topic(content) {
            lock(&self.inputs).insert((view.room.clone(), operation.clone()), text);
        }
    }

    pub async fn finished(
        &self,
        view: &SessionConfig,
        operation: &OperationId,
        outcome: TurnOutcome,
    ) {
        let text = lock(&self.inputs).remove(&(view.room.clone(), operation.clone()));
        let (Some(text), TurnOutcome::Completed) = (text, outcome) else {
            return;
        };
        // Snapshot the model before this turn releases the conversation to /model or /agent.
        match self.titles.already_named(&self.kit, &view.room).await {
            Ok(false) => {}
            Ok(true) => return,
            Err(error) => {
                record_failure(&self.profiles, &view.room, &error);
                return;
            }
        }
        match self
            .profiles
            .title_provider(view.room.as_str(), view.agent.as_str())
            .await
        {
            Ok((provider, max_tokens)) => self.titles.schedule(
                self.kit.clone(),
                self.profiles.clone(),
                view.room.clone(),
                Proposal {
                    provider,
                    max_tokens,
                    text,
                },
            ),
            Err(_) => record_failure(&self.profiles, &view.room, "Title model unavailable"),
        }
    }
}

// All reasons are generated locally. Neither source text nor provider error bodies enter diagnostics.
fn record_failure(profiles: &Profiles, room: &RoomId, reason: &str) {
    profiles.diagnostics.record(
        "session_title_failed",
        serde_json::json!({"room":room, "reason":reason}),
        None,
    );
}
