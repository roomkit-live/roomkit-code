//! Host ownership outlives agent replacements and is joined at application exit.
use super::super::*;
use roomkit_ai::{ToolError, ToolFuture, ToolHandler};
use roomkit_mcp::{AsToolHandlerOptions, McpError, McpToolProvider};
use std::collections::BTreeSet;
use tokio_util::sync::CancellationToken;

pub(super) struct Connection {
    pub server: Option<McpToolProvider>,
    pub status: String,
}
impl Connection {
    pub fn status(&self) -> &str {
        match &self.server {
            Some(server) if server.connected() => "Connected",
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

    pub async fn selected(&self, profile: &model::Agent) -> Result<Vec<McpTools>, String> {
        let connections = self.connections.lock().await;
        let mut selected = Vec::new();
        for (id, names) in &profile.mcp {
            let Some(server) = connections
                .get(id)
                .and_then(|entry| entry.server.as_ref())
                .filter(|s| s.connected())
            else {
                continue;
            };
            // A reconnected server may remove tools. Never silently select new ones.
            let tools: Vec<AiTool> = server
                .get_tools()
                .unwrap_or_default()
                .into_iter()
                .filter(|tool| names.contains(&tool.name))
                .map(|tool| AiTool {
                    name: alias(id, &tool.name),
                    ..tool
                })
                .collect();
            if tools.is_empty() {
                continue;
            }
            let handler = server
                .as_tool_handler(AsToolHandlerOptions::default())
                .map_err(|_| "Invalid MCP tool selection")?;
            let served = tools.iter().map(|tool| tool.name.clone()).collect();
            selected.push(McpTools {
                label: id.clone(),
                instructions: server.instructions().ok().flatten(),
                tools,
                handler: only(served, handler),
            });
        }
        Ok(selected)
    }
}

/// The tools of one MCP server an agent selected, as the model calls them,
/// `mcp__<server>__<tool>`, and what serves them.
#[derive(Clone)]
pub(crate) struct McpTools {
    /// The server's ID, its label in the aliases and in the instructions.
    pub label: String,
    /// What the server published on how to use its tools.
    pub instructions: Option<String>,
    pub tools: Vec<AiTool>,
    pub handler: ToolHandler,
}

/// The name the model calls the tool `tool` of the server `id` by.
fn alias(id: &str, tool: &str) -> String {
    format!("mcp__{id}__{tool}")
}

/// `handler`, serving only the calls of `served`: another server's alias,
/// a tool of this server the agent did not select, or a bare name a
/// built-in tool also bears, goes to the next handler.
fn only(served: BTreeSet<String>, handler: ToolHandler) -> ToolHandler {
    ToolHandler::new(move |name, arguments| -> ToolFuture {
        if served.contains(&name) {
            return handler.call(name, arguments);
        }
        Box::pin(async move {
            Err(ToolError::unserved(format!(
                "MCP tool {name:?} is not selected"
            )))
        })
    })
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

impl Profiles {
    /// Both form saves and explicit connections use cancellable host work.
    pub(super) fn connection_execution(
        self: &Arc<Self>,
        kit: &RoomKit<SqliteStore>,
        current: SessionConfig,
        id: String,
        connector: Option<McpToolProvider>,
    ) -> roomkit_tui_engine::CommandResult {
        use roomkit_tui::protocol::{Item, Message, RunOutcome};
        use roomkit_tui_engine::{CommandExecution, CommandResult};
        let profiles = self.clone();
        let kit = kit.clone();
        CommandResult::Execute(CommandExecution::new(
            format!("mcp-{}", uuid::Uuid::new_v4()).as_str().into(),
            move |mut context| async move {
                let runtime = &profiles.mcp_runtime;
                let _operation = runtime.operation.lock().await;
                if runtime.stop.is_cancelled() {
                    return Ok(RunOutcome::Interrupted);
                }
                let previous = runtime.connections.lock().await.remove(&id);
                if let Some(old) = previous.and_then(|c| c.server) {
                    old.close().await;
                }
                let activate = connector.is_some();
                let mut error = None;
                let mut outcome = RunOutcome::Completed;
                let connection = if let Some(connector) = connector {
                    profiles.diagnostics.record(
                        "mcp_connecting",
                        serde_json::json!({"server":id}),
                        None,
                    );
                    let stopped = async {
                        tokio::select! { () = context.cancelled() => {}, () = runtime.stop.cancelled() => {} }
                    };
                    match connector.connect_until(stopped).await {
                        Ok(()) => Connection {
                            server: Some(connector),
                            status: "Connected".into(),
                        },
                        Err(cause) => {
                            let message = failure(&cause).to_string();
                            if matches!(cause, McpError::Cancelled) {
                                outcome = RunOutcome::Interrupted;
                            } else {
                                error = Some(format!(
                                    "{id}: {message}. Configuration is saved; open /mcp to edit or reconnect."
                                ));
                            }
                            Connection {
                                server: None,
                                status: message,
                            }
                        }
                    }
                } else {
                    Connection {
                        server: None,
                        status: "Disconnected".into(),
                    }
                };
                let connected = connection.server.is_some();
                profiles.diagnostics.record(
                    "mcp_connection",
                    serde_json::json!({"server":id,"status":connection.status()}),
                    None,
                );
                runtime
                    .connections
                    .lock()
                    .await
                    .insert(id.clone(), connection);
                if !runtime.stop.is_cancelled()
                    && let Some(note) = profiles
                        .refresh_mcp(&kit, &current, &id, activate && connected)
                        .await?
                {
                    // Only exceptional setup results need a transcript note; the
                    // normal path finishes with the existing execution status.
                    context.publish(Item::Message(Message::new(
                        format!("mcp-{}", uuid::Uuid::new_v4()).as_str(),
                        "mcp",
                        note,
                    )));
                }
                if let Some(error) = error {
                    Err(error)
                } else {
                    Ok(outcome)
                }
            },
        ))
    }

    async fn refresh_mcp(
        &self,
        kit: &RoomKit<SqliteStore>,
        current: &SessionConfig,
        id: &str,
        activate: bool,
    ) -> Result<Option<String>, String> {
        let mut state = self.state.lock().await;
        let _lease = store::edit_lease(&self.state_dir)?;
        state.reload_agents()?;
        let Some(mut profile) = state.agent(current.agent.as_str()) else {
            return Ok(activate.then(||format!("MCP server {id} connected. Configure an agent with /agents, then choose its tools in /mcp.")));
        };
        let mut changed = false;
        let mut note = None;
        if activate && !profile.mcp.contains_key(id) {
            let connections = self.mcp_runtime.connections.lock().await;
            if let Some(server) = connections
                .get(id)
                .and_then(|c| c.server.as_ref())
                .filter(|s| s.connected())
            {
                let tools = super::ui::available(id, server);
                let capacity = 64usize.saturating_sub(profile.mcp.values().map(Vec::len).sum());
                let names = tools
                    .iter()
                    .take(capacity)
                    .map(|t| t.name.clone())
                    .collect::<Vec<_>>();
                if names.len() < tools.len() {
                    note = Some(format!(
                        "MCP server {id} connected: {} of {} tools enabled for {} (64 MCP tools maximum per agent). Adjust the selection in /mcp.",
                        names.len(),
                        tools.len(),
                        profile.id
                    ));
                }
                profile.mcp.insert(id.into(), names);
                changed = true;
            }
        }
        profile.validate()?;
        state.providers.reload()?;
        let provider = state
            .providers
            .value
            .profiles
            .get(&profile.provider)
            .ok_or("Unknown provider")?;
        let (agent, tools) = self.assemble(&profile, provider).await?;
        if changed {
            state.update_agent(profile.clone())?;
            self.diagnostics.record("mcp_selection",serde_json::json!({"agent":profile.id,"server":id,"tools":profile.mcp.get(id).map_or(0,Vec::len),"automatic":true}),None);
        }
        self.installed(kit, &mut state, &profile.id, agent, tools)
            .await?;
        Ok(note)
    }
}
