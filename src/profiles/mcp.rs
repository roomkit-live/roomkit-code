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

/// The first word of `/mcp`'s arguments: what it is asked to do.
fn action_of(arguments: &str) -> &str {
    arguments
        .trim()
        .split([':', ' '])
        .find(|part| !part.is_empty())
        .unwrap_or("list")
}

/// Whether `command` writes the profile files.
fn mutating(command: &Command) -> bool {
    match command {
        Command::SubmitForm { .. } => true,
        Command::Invoke { arguments, .. } => matches!(
            action_of(arguments),
            "confirm-delete" | "enable" | "disable" | "toggle"
        ),
        _ => false,
    }
}

impl Profiles {
    pub async fn mcp(
        self: &Arc<Self>,
        kit: &RoomKit<SqliteStore>,
        request: Request,
        current: SessionConfig,
    ) -> Result<CommandResult, String> {
        let mut state = self.state.lock().await;
        // Only what writes the profile files holds the cross-process edit
        // lease: a menu left open here must not block an edit elsewhere.
        let _lease = mutating(&request.command)
            .then(|| store::edit_lease(&self.state_dir))
            .transpose()?;
        if let Command::SubmitForm { form, values, .. } = request.command {
            let (server, connector) =
                ui::save(&mut state, &current, &form, &values, &self.workspace)?;
            let http = server.transport.is_http();
            return Ok(self.connection_execution(kit, current, server.id, Some(connector), http));
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
            let revision = parts.get(2).copied().unwrap_or_default();
            return self.delete_server(&mut state, &current, id, revision).await;
        }
        state.pending_mcp = None;
        // Closing the panel reads nothing: a damaged mcp.json must not keep
        // it open.
        if action == "close" {
            return Ok(CommandResult::Events(Vec::new()));
        }
        state.mcp.reload()?;
        state.mcp.value.validate()?;
        state.reload_agents()?;
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
                self.refuse_while_held(id).await?;
                ui::confirm_delete(&mut state, &current, &server)
            }
            "connect" | "disconnect" => {
                self.reconnect(kit, current, &server, action == "connect")
                    .await
            }
            "enable" | "disable" | "toggle" => {
                self.select_tools(kit, &mut state, &current, &server, &parts)
                    .await
            }
            _ => {
                Err("Use /mcp to configure servers, connect and select tools for this agent".into())
            }
        }
    }
}

impl Profiles {
    /// Refuses to delete server `id` while this application holds a
    /// connection to it.
    async fn refuse_while_held(&self, id: &str) -> Result<(), String> {
        let connections = self.mcp_runtime.connections.lock().await;
        if connections.get(id).is_some_and(|c| c.server.is_some()) {
            return Err("Disconnect this MCP server before deleting it".into());
        }
        Ok(())
    }

    /// `/mcp confirm-delete:<id>:<revision>`: deletes the server the
    /// person confirmed, as it was when they were asked.
    async fn delete_server(
        &self,
        state: &mut State,
        current: &SessionConfig,
        id: &str,
        revision: &str,
    ) -> Result<CommandResult, String> {
        self.refuse_while_held(id).await?;
        let result = ui::delete(state, current, id, revision)?;
        self.mcp_runtime.connections.lock().await.remove(id);
        Ok(result)
    }

    /// `/mcp connect:<id>` and `disconnect:<id>`: the cancellable work that
    /// opens `server`'s connection, or closes it.
    async fn reconnect(
        self: &Arc<Self>,
        kit: &RoomKit<SqliteStore>,
        current: SessionConfig,
        server: &config::Server,
        connect: bool,
    ) -> Result<CommandResult, String> {
        let id = server.id.as_str();
        let command = if connect {
            Some(server.connection(&self.workspace)?)
        } else {
            None
        };
        let connections = self.mcp_runtime.connections.lock().await;
        let live = connections
            .get(id)
            .is_some_and(|c| c.server.as_ref().is_some_and(|s| s.connected()));
        drop(connections);
        if connect && live {
            return Err("This MCP server is already connected".into());
        }
        let http = server.transport.is_http();
        Ok(self.connection_execution(kit, current, id.to_string(), command, http))
    }

    /// `/mcp enable:<id>`, `disable:<id>` and `toggle:<id>:<tool>`: changes
    /// which of `server`'s tools the current agent is served, then shows
    /// the server again.
    async fn select_tools(
        &self,
        kit: &RoomKit<SqliteStore>,
        state: &mut State,
        current: &SessionConfig,
        server: &config::Server,
        parts: &[&str],
    ) -> Result<CommandResult, String> {
        let (action, id) = (
            parts.first().copied().unwrap_or_default(),
            server.id.as_str(),
        );
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
                let all = available.iter().map(|t| t.name.clone()).collect();
                agent.mcp.insert(id.into(), all);
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
        self.installed(kit, state, &agent.id, runtime, tools)
            .await?;
        let selected = agent.mcp.get(id).map_or(0, Vec::len);
        let fields = serde_json::json!({"agent": agent.id, "server": id, "tools": selected});
        self.diagnostics.record("mcp_selection", fields, None);
        let page = parts.get(3).and_then(|p| p.parse().ok()).unwrap_or(0);
        let toggled = action == "toggle";
        ui::detail(state, current, &self.mcp_runtime, server, page, toggled).await
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
