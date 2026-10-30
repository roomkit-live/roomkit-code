//! MCP configuration, explicit connections and per-agent exposure belong to the
//! host. Protocol, aliases, selection enforcement and process cleanup are supplied
//! by roomkit-mcp; there is no application tool implementation here.
mod config;
mod runtime;
mod ui;

use super::*;
pub(super) use config::Servers;
pub(crate) use config::valid_id;
use roomkit_cli_roomkit::{CommandExecution, CommandResult};
use roomkit_tui::protocol::{Command, Request, RunOutcome};
pub(super) use runtime::Runtime;
use runtime::{Connection, failure};
pub(super) use ui::Pending;

impl Profiles {
    pub async fn mcp(
        self: &Arc<Self>,
        kit: &RoomKit<SqliteStore>,
        request: Request,
        current: SessionConfig,
    ) -> Result<CommandResult, String> {
        let mut state = self.state.lock().await;
        let _lease = store::edit_lease(&self.state_dir)?;
        if let Command::SubmitForm { form, values, .. } = request.command {
            return ui::save(&mut state, &current, &form, &values);
        }
        let Command::Invoke { arguments, .. } = request.command else {
            return Err("Unsupported MCP request".into());
        };
        let arguments = arguments.trim();
        let parts = arguments
            .split([':', ' '])
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        let action = parts.first().copied().unwrap_or("list");
        let id = parts.get(1).copied().unwrap_or_default();
        if action == "confirm-delete" {
            if self
                .mcp_runtime
                .connections
                .lock()
                .await
                .get(id)
                .is_some_and(|entry| entry.server.is_some())
            {
                return Err("Disconnect this MCP server before deleting it".into());
            }
            let result = ui::delete(
                &mut state,
                &current,
                id,
                parts.get(2).copied().unwrap_or_default(),
            )?;
            self.mcp_runtime.connections.lock().await.remove(id);
            return Ok(result);
        }
        state.pending_mcp = None;
        state.mcp.reload()?;
        state.mcp.value.validate()?;
        state.project.reload()?;
        if state.project.value.schema != 2 {
            return Err("Unsupported agent configuration version".into());
        }
        if action == "close" {
            return Ok(CommandResult::Events(Vec::new()));
        }
        if action == "list" {
            return Ok(ui::list(&state, &current, &self.mcp_runtime).await);
        }
        if action == "add" {
            return ui::form(&mut state, &current, None);
        }
        let server = state
            .mcp
            .value
            .servers
            .get(id)
            .ok_or("Unknown MCP server; open /mcp")?
            .clone();
        match action {
            "show" | "tools" => {
                ui::detail(
                    &state,
                    &current,
                    &self.mcp_runtime,
                    &server,
                    parts.get(2).and_then(|v| v.parse().ok()).unwrap_or(0),
                    action == "tools",
                )
                .await
            }
            "edit" => {
                if self
                    .mcp_runtime
                    .connections
                    .lock()
                    .await
                    .get(id)
                    .is_some_and(|c| c.server.is_some())
                {
                    return Err("Disconnect this MCP server before editing its command".into());
                }
                ui::form(&mut state, &current, Some(&server))
            }
            "delete" => {
                if self
                    .mcp_runtime
                    .connections
                    .lock()
                    .await
                    .get(id)
                    .is_some_and(|c| c.server.is_some())
                {
                    return Err("Disconnect this MCP server before deleting it".into());
                }
                ui::confirm_delete(&mut state, &current, &server)
            }
            "connect" | "disconnect" => {
                let command = if action == "connect" {
                    Some(server.command(&self.workspace)?)
                } else {
                    None
                };
                if command.is_some()
                    && self
                        .mcp_runtime
                        .connections
                        .lock()
                        .await
                        .get(id)
                        .is_some_and(|c| c.server.as_ref().is_some_and(|s| s.is_connected()))
                {
                    return Err("This MCP server is already connected".into());
                }
                let profiles = self.clone();
                let kit = kit.clone();
                let id = id.to_string();
                // The adapter owns admission, busy state and Esc. No network or
                // process startup is awaited in the UI command handler.
                Ok(CommandResult::Execute(CommandExecution::new(
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
                        let mut error = None;
                        let mut outcome = RunOutcome::Completed;
                        let connection = if let Some(command) = command {
                            profiles.diagnostics.record(
                                "mcp_connecting",
                                serde_json::json!({"server":id}),
                                None,
                            );
                            let stopped = async {
                                tokio::select! { () = context.cancelled() => {}, () = runtime.stop.cancelled() => {} }
                            };
                            match command.connect_until(stopped).await {
                                Ok(server) => Connection {
                                    server: Some(server),
                                    status: "Connected".into(),
                                },
                                Err(cause) => {
                                    let message = failure(&cause).to_string();
                                    if matches!(cause, roomkit_mcp::McpError::Cancelled) {
                                        outcome = RunOutcome::Interrupted;
                                    } else {
                                        error = Some(format!(
                                            "{id}: {message}. Open /mcp to inspect or edit this server."
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
                        profiles.diagnostics.record(
                            "mcp_connection",
                            serde_json::json!({"server":id,"status":connection.status()}),
                            None,
                        );
                        runtime.connections.lock().await.insert(id, connection);
                        if !runtime.stop.is_cancelled() {
                            // Idle agent slots are rebuilt when selected. The active
                            // participant receives this connection's current catalogue.
                            let mut state = profiles.state.lock().await;
                            let profile = state
                                .project
                                .value
                                .agents
                                .get(current.agent.as_str())
                                .cloned();
                            if let Some(profile) = profile {
                                let provider = state
                                    .providers
                                    .value
                                    .profiles
                                    .get(&profile.provider)
                                    .ok_or("Unknown provider")?;
                                let (agent, tools) = profiles.assemble(&profile, provider).await?;
                                state.install(&kit, &profile.id, agent, tools)?;
                            }
                        }
                        if let Some(error) = error {
                            Err(error)
                        } else {
                            Ok(outcome)
                        }
                    },
                )))
            }
            "enable" | "disable" | "toggle" => {
                state.pending = None;
                state.pending_delete = None;
                let mut agent = state
                    .project
                    .value
                    .agents
                    .get(current.agent.as_str())
                    .ok_or("Configure an agent first")?
                    .clone();
                if action == "disable" {
                    agent.mcp.remove(id);
                } else {
                    let connections = self.mcp_runtime.connections.lock().await;
                    let live = connections
                        .get(id)
                        .and_then(|c| c.server.as_ref())
                        .filter(|s| s.is_connected())
                        .ok_or("Connect the server before selecting its tools")?;
                    let available = ui::available(id, live);
                    if action == "enable" {
                        agent.mcp.insert(
                            id.into(),
                            available.iter().map(|t| t.name.clone()).collect(),
                        );
                    } else {
                        let name = parts.get(2).copied().ok_or("Choose an MCP tool")?;
                        if !available.iter().any(|t| t.name == name) {
                            return Err("Tool is no longer available".into());
                        }
                        let names = agent.mcp.entry(id.into()).or_default();
                        if names.iter().any(|n| n == name) {
                            names.retain(|n| n != name);
                        } else {
                            names.push(name.into());
                            names.sort();
                        }
                        if names.is_empty() {
                            agent.mcp.remove(id);
                        }
                    }
                }
                agent.validate()?;
                let provider = state
                    .providers
                    .value
                    .profiles
                    .get(&agent.provider)
                    .ok_or("Unknown provider")?;
                let (runtime, tools) = self.assemble(&agent, provider).await?;
                let mut project = state.project.value.clone();
                project.agents.insert(agent.id.clone(), agent.clone());
                state.project.save(project)?;
                state.install(kit, &agent.id, runtime, tools)?;

                self.diagnostics.record("mcp_selection", serde_json::json!({"agent":agent.id,"server":id,"tools":agent.mcp.get(id).map_or(0,Vec::len)}), None);
                let page = parts.get(3).and_then(|p| p.parse().ok()).unwrap_or(0);
                ui::detail(
                    &state,
                    &current,
                    &self.mcp_runtime,
                    &server,
                    page,
                    action == "toggle",
                )
                .await
            }
            _ => {
                Err("Use /mcp to configure servers, connect and select tools for this agent".into())
            }
        }
    }
}
