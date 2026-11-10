//! MCP configuration, connection workflow and per-agent exposure belong to the
//! host: it names the selected tools `mcp__<server>__<tool>`, serves only those,
//! and closes the servers. The protocol and each stdio server's process group are
//! roomkit-mcp's; there is no application tool implementation here.
mod config;
mod runtime;
mod ui;

use super::*;
pub(super) use config::Servers;
pub(crate) use config::valid_id;
use roomkit_tui::protocol::{Command, Request};
use roomkit_tui_engine::CommandResult;
pub(crate) use runtime::McpTools;
pub(super) use runtime::Runtime;
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
            let (server, connector) =
                ui::save(&mut state, &current, &form, &values, &self.workspace)?;
            return Ok(self.connection_execution(kit, current, server.id, Some(connector)));
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
        state.reload_agents()?;
        if action == "close" {
            return Ok(CommandResult::Events(Vec::new()));
        }
        if action == "list" {
            return Ok(ui::list(&state, &current, &self.mcp_runtime).await);
        }
        if matches!(action, "add" | "add-http") {
            return ui::form(&mut state, &current, None, action == "add-http");
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
            "edit" => ui::form(
                &mut state,
                &current,
                Some(&server),
                server.transport.is_http(),
            ),
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
                    Some(server.connection(&self.workspace)?)
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
                        .is_some_and(|c| c.server.as_ref().is_some_and(|s| s.connected()))
                {
                    return Err("This MCP server is already connected".into());
                }
                Ok(self.connection_execution(kit, current, id.to_string(), command))
            }

            "enable" | "disable" | "toggle" => {
                state.pending = None;
                state.pending_delete = None;
                let mut agent = state
                    .agent(current.agent.as_str())
                    .ok_or("Configure an agent first")?
                    .clone();
                if action == "disable" {
                    agent.mcp.insert(id.into(), Vec::new());
                } else {
                    let connections = self.mcp_runtime.connections.lock().await;
                    let live = connections
                        .get(id)
                        .and_then(|c| c.server.as_ref())
                        .filter(|s| s.connected())
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
                state.update_agent(agent.clone())?;
                self.installed(kit, &mut state, &agent.id, runtime, tools)
                    .await?;

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

pub(super) async fn sidebar(
    state: &State,
    current: &SessionConfig,
    runtime: &Runtime,
) -> roomkit_tui::protocol::SidebarSection {
    use roomkit_tui::protocol::{SidebarContent, SidebarEntry, SidebarSection, SidebarTone};
    let connections = runtime.connections.lock().await;
    let agent = state.agent(current.agent.as_str());
    let mut entries = Vec::new();
    for id in state.mcp.value.servers.keys() {
        let connection = connections.get(id);
        let live = connection
            .and_then(|c| c.server.as_ref())
            .filter(|s| s.connected());
        let names = agent
            .as_ref()
            .and_then(|a| a.mcp.get(id))
            .map_or(&[][..], Vec::as_slice);
        let available = live.and_then(|s| s.get_tools().ok()).unwrap_or_default();
        let enabled = names
            .iter()
            .filter(|name| available.iter().any(|tool| &tool.name == *name))
            .count();
        entries.push(
            SidebarEntry::new(format!("{} {id}", if live.is_some() { "●" } else { "○" }))
                .with_detail(format!(
                    "{} · {enabled} active tools",
                    connection.map_or("Disconnected", |c| c.status())
                ))
                .with_tone(if live.is_some() {
                    SidebarTone::Success
                } else {
                    SidebarTone::Warning
                })
                .with_action(super::sidebar::action("mcp", format!("show:{id}"))),
        );
    }
    if entries.is_empty() {
        entries.push(SidebarEntry::new("No servers configured").with_tone(SidebarTone::Muted));
    }
    SidebarSection::new("MCP servers", SidebarContent::Entries { entries })
        .with_action(super::sidebar::action("mcp", ""))
}
