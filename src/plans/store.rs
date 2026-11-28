use roomkit_tools_core::ToolError;
use roomkit_tools_plans::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const MAX_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct Plans {
    root: PathBuf,
    // Held inside the blocking operation until the atomic write and notification
    // finish, even if the caller drops its future after dispatch.
    lock: Arc<Mutex<()>>,
    changes: watch::Sender<()>,
    closed: Arc<AtomicBool>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Document {
    version: u8,
    room: String,
    agent: String,
    pub selected: Option<String>,
    pub plans: BTreeMap<String, Plan>,
}

/// The plans could not be read or written, for `cause`: which of the file,
/// its directory, its permissions or its content, so the person who opens
/// `/plans` knows what to repair. Never the file's content.
fn unavailable(cause: &str) -> ToolError {
    ToolError::UnsupportedResource(format!(
        "Plan storage is unavailable: {cause}. No plan data was displayed"
    ))
}
fn cancelled(token: &CancellationToken) -> Result<(), ToolError> {
    if token.is_cancelled() {
        Err(ToolError::Cancelled)
    } else {
        Ok(())
    }
}
fn owned<'a>(doc: &'a Document, id: &str) -> Result<&'a Plan, ToolError> {
    doc.plans.get(id).ok_or_else(|| {
        ToolError::AccessRefused("Unknown plan in this conversation and agent".into())
    })
}
fn revision(plan: &Plan, expected: u64) -> Result<(), ToolError> {
    if plan.revision == expected {
        Ok(())
    } else {
        Err(ToolError::AccessRefused(
            "Plan revision conflict; use plan_read before retrying".into(),
        ))
    }
}

impl Plans {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            lock: Arc::default(),
            changes: watch::channel(()).0,
            closed: Arc::default(),
        }
    }
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }
    pub async fn close(&self) -> bool {
        self.closed.store(true, Ordering::SeqCst);
        let lock = self.lock.clone();
        tokio::task::spawn_blocking(move || lock.lock().is_ok())
            .await
            .unwrap_or(false)
    }

    async fn access<T: Send + 'static>(
        &self,
        scope: &PlanScope,
        cancel: &CancellationToken,
        apply: impl FnOnce(&mut Document) -> Result<T, ToolError> + Send + 'static,
    ) -> Result<T, ToolError> {
        scope.validate()?;
        cancelled(cancel)?;
        let this = self.clone();
        let scope = scope.clone();
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = this.lock.lock().map_err(|_| {
                unavailable("an earlier plan operation failed while it held the storage")
            })?;
            if this.closed.load(Ordering::SeqCst) {
                return Err(ToolError::Cancelled);
            }
            cancelled(&cancel)?;
            let key = serde_json::to_vec(&(&scope.room_id, &scope.caller_id))
                .map_err(|_| unavailable("this conversation's scope cannot be encoded"))?;
            let path = this
                .root
                .join(format!("{}.json", crate::digest::sha256_hex(key)));
            let mut doc = load(&path, &scope)?;
            let before = doc.clone();
            let output = apply(&mut doc)?;
            validate(&doc, &scope)?;
            cancelled(&cancel)?;
            if before != doc {
                save(&path, &doc)?;
                this.changes.send_replace(());
            }
            Ok(output)
        })
        .await
        .map_err(|_| unavailable("the storage task stopped"))?
    }

    /// Plan `id` in `scope`, or with no ID the conversation's only plan:
    /// with several, which one is meant must be said.
    pub(crate) async fn plan(
        &self,
        scope: &PlanScope,
        id: Option<&str>,
    ) -> Result<Plan, ToolError> {
        let document = self.snapshot(scope).await?;
        let plan = match id {
            Some(id) => document.plans.get(id),
            None if document.plans.len() > 1 => {
                return Err(ToolError::InvalidInput(
                    "This conversation has several plans; give plan_id (see plan_list)".into(),
                ));
            }
            None => document.plans.values().next(),
        };
        plan.cloned().ok_or_else(|| {
            ToolError::InvalidInput(
                "No such plan in this conversation; create it with plan_create".into(),
            )
        })
    }

    pub(super) async fn snapshot(&self, scope: &PlanScope) -> Result<Document, ToolError> {
        self.access(scope, &CancellationToken::new(), |doc| Ok(doc.clone()))
            .await
    }

    /// The plans `agent` holds in `room`, the one selected first; none when
    /// they cannot be read.
    pub(crate) async fn held(&self, room: &str, agent: &str) -> Vec<Plan> {
        let scope = PlanScope {
            room_id: room.into(),
            caller_id: agent.into(),
        };
        let Ok(document) = self.snapshot(&scope).await else {
            return Vec::new();
        };
        let selected = document.selected;
        let mut plans: Vec<Plan> = document.plans.into_values().collect();
        plans.sort_by_key(|plan| selected.as_deref() != Some(plan.plan_id.as_str()));
        plans
    }

    pub async fn select(&self, scope: &PlanScope, id: &str) -> Result<(), ToolError> {
        let id = id.to_owned();
        self.access(scope, &CancellationToken::new(), move |doc| {
            owned(doc, &id)?;
            doc.selected = Some(id);
            Ok(())
        })
        .await
    }
}

fn validate(doc: &Document, scope: &PlanScope) -> Result<(), ToolError> {
    let selected = doc.selected.as_ref();
    let causes = [
        (doc.version != 1, "its file is of another version"),
        (
            doc.room != scope.room_id,
            "its file holds another conversation's plans",
        ),
        (
            doc.agent != scope.caller_id,
            "its file holds another agent's plans",
        ),
        (
            doc.plans.len() > 64,
            "its file holds more plans than a conversation keeps",
        ),
        (
            selected.is_some_and(|id| !doc.plans.contains_key(id)),
            "the stored selection names a plan that is not stored",
        ),
    ];
    if let Some((_, cause)) = causes.iter().find(|(wrong, _)| *wrong) {
        return Err(unavailable(cause));
    }
    for (id, plan) in &doc.plans {
        plan.validate()
            .map_err(|_| unavailable("a stored plan is not valid"))?;
        if id != &plan.plan_id {
            return Err(unavailable("a stored plan is kept under another plan's ID"));
        }
    }
    Ok(())
}

fn load(path: &Path, scope: &PlanScope) -> Result<Document, ToolError> {
    private_directory(
        path.parent()
            .ok_or_else(|| unavailable("its file has no directory"))?,
    )?;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Document {
                version: 1,
                room: scope.room_id.clone(),
                agent: scope.caller_id.clone(),
                selected: None,
                plans: BTreeMap::new(),
            });
        }
        Err(_) => return Err(unavailable("its file cannot be inspected")),
    };
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return Err(unavailable("its file is not a regular file under 5 MiB"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(unavailable("its file is readable by others (chmod 600)"));
        }
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| unavailable("its file cannot be opened"))?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable("its file cannot be read"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(unavailable("its file grew past 5 MiB"));
    }
    let doc = serde_json::from_slice(&bytes)
        .map_err(|_| unavailable("its file does not hold plan data"))?;
    validate(&doc, scope)?;
    Ok(doc)
}

fn save(path: &Path, doc: &Document) -> Result<(), ToolError> {
    let parent = path
        .parent()
        .ok_or_else(|| unavailable("its file has no directory"))?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(parent)
        .map_err(|_| unavailable("its directory cannot be created"))?;
    private_directory(parent)?;
    let bytes = serde_json::to_vec(doc).map_err(|_| unavailable("the plans cannot be encoded"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(ToolError::LimitExceeded("Plan scope exceeds 5 MiB".into()));
    }
    // A lost receipt after replacement must be recovered with read/list.
    crate::storage::write_atomic(path, &bytes)
        .map_err(|_| unavailable("its file cannot be written"))
}

fn private_directory(path: &Path) -> Result<(), ToolError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(unavailable("its directory cannot be inspected")),
    };
    if !meta.is_dir() {
        return Err(unavailable("its directory is not a directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(unavailable(
                "its directory is readable by others (chmod 700)",
            ));
        }
    }
    Ok(())
}

impl PlanBackend for Plans {
    fn create<'a>(
        &'a self,
        scope: &'a PlanScope,
        input: &'a PlanCreateInput,
        cancel: &'a CancellationToken,
    ) -> PlanFuture<'a, Plan> {
        Box::pin(async move {
            input.validate()?;
            let input = input.clone();
            self.access(scope, cancel, move |doc| {
                if doc.plans.len() >= 64 {
                    return Err(ToolError::LimitExceeded(
                        "This agent already has 64 plans in this conversation".into(),
                    ));
                }
                let plan = Plan {
                    plan_id: uuid::Uuid::new_v4().to_string(),
                    revision: 1,
                    title: input.title,
                    objective: input.objective,
                    steps: input.steps,
                };
                plan.validate()?;
                doc.plans.insert(plan.plan_id.clone(), plan.clone());
                if doc.plans.len() == 1 {
                    doc.selected = Some(plan.plan_id.clone());
                }
                Ok(plan)
            })
            .await
        })
    }
    fn read<'a>(
        &'a self,
        scope: &'a PlanScope,
        input: &'a PlanReadInput,
        cancel: &'a CancellationToken,
    ) -> PlanFuture<'a, Plan> {
        Box::pin(async move {
            input.validate()?;
            let id = input.plan_id.clone();
            self.access(scope, cancel, move |doc| owned(doc, &id).cloned())
                .await
        })
    }
    fn list<'a>(
        &'a self,
        scope: &'a PlanScope,
        input: &'a PlanListInput,
        cancel: &'a CancellationToken,
    ) -> PlanFuture<'a, PlanListResult> {
        Box::pin(async move {
            input.validate()?;
            self.access(scope, cancel, |doc| {
                Ok(PlanListResult {
                    plans: doc
                        .plans
                        .values()
                        .map(|p| PlanSummary {
                            plan_id: p.plan_id.clone(),
                            revision: p.revision,
                            title: p.title.clone(),
                            total_steps: p.steps.len(),
                            completed_steps: p
                                .steps
                                .iter()
                                .filter(|s| s.status == StepStatus::Completed)
                                .count(),
                        })
                        .collect(),
                })
            })
            .await
        })
    }
    fn update<'a>(
        &'a self,
        scope: &'a PlanScope,
        input: &'a PlanUpdateInput,
        cancel: &'a CancellationToken,
    ) -> PlanFuture<'a, Plan> {
        Box::pin(async move {
            input.validate()?;
            let input = input.clone();
            self.access(scope, cancel, move |doc| {
                let current = owned(doc, &input.plan_id)?;
                revision(current, input.expected_revision)?;
                let mut next = current.clone();
                if let Some(title) = input.title {
                    next.title = title;
                }
                if let Some(objective) = input.objective {
                    next.objective = objective;
                }
                if let Some(steps) = input.steps {
                    next.steps = steps;
                }
                if next != *current {
                    next.revision += 1;
                }
                next.validate()?;
                doc.plans.insert(next.plan_id.clone(), next.clone());
                Ok(next)
            })
            .await
        })
    }
    fn delete<'a>(
        &'a self,
        scope: &'a PlanScope,
        input: &'a PlanDeleteInput,
        cancel: &'a CancellationToken,
    ) -> PlanFuture<'a, PlanDeleteResult> {
        Box::pin(async move {
            input.validate()?;
            let input = input.clone();
            self.access(scope, cancel, move |doc| {
                let plan = owned(doc, &input.plan_id)?;
                revision(plan, input.expected_revision)?;
                let result = PlanDeleteResult {
                    plan_id: plan.plan_id.clone(),
                    revision: plan.revision,
                    deleted: true,
                };
                doc.plans.remove(&input.plan_id);
                if doc.selected.as_ref() == Some(&input.plan_id) {
                    doc.selected = None;
                }
                Ok(result)
            })
            .await
        })
    }
}
