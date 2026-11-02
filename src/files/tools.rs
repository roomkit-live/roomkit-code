//! Observe file effects where tools execute, after their approval.

use std::sync::Arc;

use roomkit::model::Metadata;
use roomkit_ai::{AiTool, Tool, ToolFuture, ToolResult, current_tool_call};
use roomkit_tools_fs::{PatchOperation, PatchResult};
use serde_json::Value;

use super::{Change, Effect, Files, Snapshot};

pub(crate) struct Tracked {
    inner: Arc<dyn Tool>,
    /// The tool `inner` serves: a call to another passes on unobserved.
    name: String,
    files: Arc<Files>,
}

impl Files {
    /// Record what `write`, `edit`, `patch` and `read` report, and compare the
    /// project around each `shell` command. Other tools pass through unchanged.
    pub fn wrap(self: &Arc<Self>, inner: Arc<dyn Tool>) -> Arc<dyn Tool> {
        Arc::new(Tracked {
            name: inner.definition().name,
            inner,
            files: self.clone(),
        })
    }
}

impl Tool for Tracked {
    fn definition(&self) -> AiTool {
        self.inner.definition()
    }

    fn handler(&self, name: String, arguments: Metadata) -> ToolFuture {
        let room = current_tool_call().and_then(|call| call.room_id);
        let Some(room) = room.filter(|_| name == self.name) else {
            return self.inner.handler(name, arguments);
        };
        let room = room.to_string();
        let files = self.files.clone();
        if name == "shell" {
            let call = self.inner.handler(name, arguments);
            return Box::pin(async move {
                // A failed command may still have changed files: compare either way.
                let comparison = Comparison {
                    before: Some(files.snapshot().await),
                    files,
                    room,
                };
                let result = call.await;
                comparison.finish().await;
                result
            });
        }
        let call = self.inner.handler(name.clone(), arguments);
        Box::pin(async move {
            let result = call.await;
            if let Ok(ToolResult::Json(output)) = &result {
                let (effects, read) = reported(&name, &output["data"]);
                if !effects.is_empty() || read.is_some() {
                    files.observed(&room, effects, read).await;
                }
            }
            result
        })
    }
}

/// The comparison around one shell call. Cancelling the turn or a tool timeout
/// drops the call, possibly after the command changed files: the comparison
/// then still runs, in a task of its own.
struct Comparison {
    files: Arc<Files>,
    room: String,
    before: Option<Option<Snapshot>>,
}

impl Comparison {
    async fn finish(mut self) {
        if let Some(before) = self.before.take() {
            let room = std::mem::take(&mut self.room);
            let _ = self.files.compare_later(room, before).await;
        }
    }
}

impl Drop for Comparison {
    fn drop(&mut self) {
        if let Some(before) = self.before.take()
            && tokio::runtime::Handle::try_current().is_ok()
        {
            let room = std::mem::take(&mut self.room);
            drop(self.files.compare_later(room, before));
        }
    }
}

/// Effects a file tool reported, and the path it read. Previews and calls that
/// left a file unchanged report nothing.
pub(super) fn reported(name: &str, data: &Value) -> (Vec<Effect>, Option<String>) {
    let path = || data["path"].as_str().map(str::to_owned);
    let flag = |key: &str| data[key].as_bool() == Some(true);
    let effect = |path: String, change| Effect {
        path,
        change,
        shell: false,
    };
    match name {
        "write" if flag("created") => (
            path()
                .map(|p| effect(p, Change::Created))
                .into_iter()
                .collect(),
            None,
        ),
        "write" | "edit" if flag("changed") => (
            path()
                .map(|p| effect(p, Change::Modified))
                .into_iter()
                .collect(),
            None,
        ),
        "patch" => {
            let Ok(result) = serde_json::from_value::<PatchResult>(data.clone()) else {
                return (Vec::new(), None);
            };
            let effects = result
                .applied
                .into_iter()
                .filter_map(|applied| {
                    let change = match applied.operation {
                        PatchOperation::Add => Change::Created,
                        PatchOperation::Update => Change::Modified,
                        PatchOperation::Delete => Change::Deleted,
                        // A move is reported as its add and delete effects.
                        PatchOperation::Move => return None,
                    };
                    Some(effect(applied.path, change))
                })
                .collect();
            (effects, None)
        }
        "read" => (Vec::new(), path()),
        _ => (Vec::new(), None),
    }
}
