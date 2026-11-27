//! Project scope, quota and backend choice belong to the host.
mod history;
mod provider;
mod scope;
#[cfg(test)]
mod tests;
mod tool;
mod view;
pub(crate) use history::History;
pub(crate) use provider::AgentProvider;
pub(crate) use view::View;

use crate::{config::Settings, profiles::mcp::Runtime};
use roomkit_tools_core::ToolError;
use roomkit_tools_memory::{FileMemory, MemoryBackend, MemoryFuture, MemoryInput, MemoryTool};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

const QUOTA: usize = 256 * 1024;

#[derive(Default, Deserialize)]
#[serde(tag = "backend", rename_all = "lowercase", deny_unknown_fields)]
pub(crate) enum Config {
    #[default]
    Markdown,
    Mcp {
        server: String,
    },
}
impl Config {
    pub fn checked(self) -> crate::Result<Self> {
        if self
            .server()
            .is_some_and(|id| !crate::profiles::mcp::valid_id(id))
        {
            return Err("Invalid [memory] server ID. Use a saved /mcp connection ID.".into());
        }
        Ok(self)
    }
    pub fn server(&self) -> Option<&str> {
        match self {
            Self::Mcp { server } => Some(server),
            Self::Markdown => None,
        }
    }
}

async fn project_root(workspace: &Path) -> Result<PathBuf, String> {
    let workspace = workspace
        .canonicalize()
        .map_err(|_| "Cannot resolve workspace for memory")?;
    // A linked worktree and the primary checkout refer to the same common directory.
    let git = tokio::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .current_dir(&workspace)
        .output()
        .await;
    if let Ok(output) = git
        && output.status.success()
    {
        let path = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStrExt;
            Path::new(std::ffi::OsStr::from_bytes(path))
        };
        #[cfg(not(unix))]
        let path = Path::new(
            std::str::from_utf8(path)
                .map_err(|_| "Invalid repository path")?
                .trim_end_matches('\r'),
        );
        return path
            .canonicalize()
            .map_err(|_| "Cannot resolve Git common directory for memory".into());
    }
    Ok(workspace)
}

pub(crate) async fn bind(
    settings: &Settings,
    runtime: &Arc<Runtime>,
) -> Result<MemoryTool, String> {
    let project = project_root(&settings.workspace).await?;
    let backend: Arc<dyn MemoryBackend> = match &settings.memory {
        Config::Markdown => Arc::new(FileMemory::new(
            crate::storage::project_directory(&settings.state_dir, &project).join("memory"),
            QUOTA,
        )),
        Config::Mcp { server } => Arc::new(RemoteMemory {
            server: server.clone(),
            scope: scope::remote(&settings.state_dir, &project)?,
            runtime: runtime.clone(),
        }),
    };
    Ok(MemoryTool::new(backend))
}

struct RemoteMemory {
    runtime: Arc<Runtime>,
    server: String,
    scope: String,
}
impl std::fmt::Debug for RemoteMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteMemory")
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}

impl MemoryBackend for RemoteMemory {
    fn execute<'a>(
        &'a self,
        input: &'a MemoryInput,
        cancel: &'a CancellationToken,
    ) -> MemoryFuture<'a> {
        Box::pin(async move {
            let server = self
                .runtime
                .memory_connection(&self.server)
                .await
                .ok_or_else(|| {
                    ToolError::InvalidInput(format!(
                        "Project memory is offline. Connect {} in /mcp.",
                        self.server
                    ))
                })?;
            let args = serde_json::Map::from_iter([
                ("scope".into(), serde_json::json!(self.scope)),
                ("max_bytes".into(), serde_json::json!(QUOTA)),
                ("input".into(), serde_json::json!(input)),
            ]);
            let raw = tokio::select! {
                () = cancel.cancelled() => return Err(ToolError::Cancelled),
                result = server.call_tool("memory",args,roomkit_mcp::CallToolOptions::default()) => result.map_err(|_| ToolError::InvalidInput("Project memory call failed; check its /mcp connection".into()))?,
            };
            let result: serde_json::Value = serde_json::from_str(&raw).map_err(|_| {
                ToolError::InvalidInput("Memory server returned invalid JSON".into())
            })?;
            if let Some(error) = result.get("error").and_then(|e| e.as_str()) {
                return Err(ToolError::InvalidInput(error.chars().take(250).collect()));
            }
            serde_json::from_value(result).map_err(|_| {
                ToolError::InvalidInput("Memory server returned an invalid note result".into())
            })
        })
    }
}
