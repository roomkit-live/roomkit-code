//! Host ownership outlives agent replacements and is joined at application exit.
use super::super::*;
use roomkit_ai::Toolset;
use roomkit_mcp::{McpError, McpServer};
use tokio_util::sync::CancellationToken;

pub(super) struct Connection {
    pub server: Option<McpServer>,
    pub status: String,
}
impl Connection {
    pub fn status(&self) -> &str {
        match &self.server {
            Some(server) if server.is_connected() => "Connected",
            Some(_) => "Disconnected · server exited or closed its transport",
            None => &self.status,
        }
    }
}

#[derive(Default)]
pub(crate) struct Runtime {
    pub(super) connections: Mutex<BTreeMap<String, Connection>>,
    pub(super) operation: Mutex<()>,
    pub(super) stop: CancellationToken,
}
impl Runtime {
    pub async fn close(&self) {
        self.stop.cancel();
        let _operation = self.operation.lock().await;
        let connections = std::mem::take(&mut *self.connections.lock().await);
        for entry in connections.into_values() {
            if let Some(server) = entry.server {
                server.close().await;
            }
        }
    }

    pub async fn selected(
        &self,
        profile: &model::Agent,
    ) -> Result<Vec<(String, McpServer)>, String> {
        let connections = self.connections.lock().await;
        let mut selected = Vec::new();
        for (id, names) in &profile.mcp {
            let Some(server) = connections
                .get(id)
                .and_then(|entry| entry.server.as_ref())
                .filter(|s| s.is_connected())
            else {
                continue;
            };
            // A reconnected server may remove tools. Never silently select new ones.
            let catalogue = server.tools();
            let available = names
                .iter()
                .filter(|name| catalogue.iter().any(|tool| &tool.name == *name))
                .cloned()
                .collect::<Vec<_>>();
            if available.is_empty() {
                continue;
            }
            let tools = server
                .clone()
                .with_label(id)
                .and_then(|s| s.with_tool_names(&available))
                .map_err(|_| "Invalid MCP tool selection")?;
            selected.push((id.clone(), tools));
        }
        Ok(selected)
    }
}

pub(super) fn failure(error: &McpError) -> &'static str {
    match error {
        McpError::Http {
            status: Some(401), ..
        } => "Authentication required or rejected · check credential environment references",
        McpError::Http {
            status: Some(403), ..
        } => "The MCP endpoint refused access · check credential permissions",
        McpError::Http {
            status: Some(404), ..
        } => "MCP endpoint not found or session expired · check the URL and reconnect",
        McpError::Http {
            status: Some(429), ..
        } => "MCP endpoint rate limit reached · try again later",
        McpError::Http { reason, .. } => reason,
        McpError::Cancelled => "Connection cancelled",
        McpError::Spawn { source, .. } if source.kind() == std::io::ErrorKind::NotFound => {
            "Program not found · check its executable and PATH"
        }
        McpError::Spawn { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied => {
            "Permission denied while starting the program"
        }
        McpError::Spawn { .. } => "Could not start the program",
        McpError::Timeout { .. } => {
            "Connection timed out · check the program, arguments and required environment"
        }
        McpError::Handshake { .. } => {
            "MCP handshake failed · the program must serve MCP over stdin/stdout"
        }
        McpError::Listing { .. } => "MCP tool discovery failed",
        _ => "Invalid MCP configuration",
    }
}
