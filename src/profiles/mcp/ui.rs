use super::super::ui::choice;
use super::super::*;
use super::{Runtime, config::Server};
use roomkit_ai::Toolset;
use roomkit_cli_roomkit::CommandResult;
use roomkit_tui::protocol::{Choice, Event, FieldKind, FormField, FormSpec, FormValues, PanelSpec};

pub(in crate::profiles) enum Pending {
    Form {
        room: String,
        form: String,
        id: Option<String>,
    },
    Delete {
        room: String,
        token: String,
        id: String,
    },
}
fn panel(
    current: &SessionConfig,
    title: String,
    body: String,
    actions: Vec<Choice>,
) -> CommandResult {
    CommandResult::Events(vec![Event::Panel {
        conversation: current.room.as_str().into(),
        panel: PanelSpec {
            id: uuid::Uuid::new_v4().to_string(),
            command: "mcp".into(),
            title,
            body,
            actions,
            dismiss: "close".into(),
        },
    }])
}
pub(super) async fn list(
    state: &State,
    current: &SessionConfig,
    runtime: &Runtime,
) -> CommandResult {
    let connections = runtime.connections.lock().await;
    let agent = state.project.value.agents.get(current.agent.as_str());
    let mut actions = vec![choice(
        "add",
        "Add a stdio server",
        "Configure a local executable; connect explicitly after saving",
    )];
    for id in state.mcp.value.servers.keys() {
        let status = connections.get(id).map_or("Disconnected", |c| c.status());
        let selected = agent.and_then(|a| a.mcp.get(id)).map_or(0, Vec::len);
        actions.push(choice(
            format!("show:{id}"),
            id,
            format!("{status} · {selected} tools selected for this agent"),
        ));
    }
    panel(current, format!("MCP · agent {}", current.agent),
        "Local servers run with your user permissions, in this workspace. Connect explicitly, then select tools for this agent. Connections last until disconnect or application exit. Ask approves every MCP tool call; Yolo runs selected tools without asking. Servers and tools are independent of the agent's model provider.".into(), actions)
}

pub(super) fn available(id: &str, server: &roomkit_mcp::McpServer) -> Vec<ToolDefinition> {
    server
        .tools()
        .into_iter()
        .filter(|tool| {
            let alias = format!("mcp__{id}__{}", tool.name);
            alias.len() <= 128
                && alias
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        })
        .collect()
}

pub(super) async fn detail(
    state: &State,
    current: &SessionConfig,
    runtime: &Runtime,
    config: &Server,
    offset: usize,
    tools_page: bool,
) -> Result<CommandResult, String> {
    let connections = runtime.connections.lock().await;
    let connection = connections.get(&config.id);
    let status = connection.map_or("Disconnected", |c| c.status());
    let live = connection
        .and_then(|c| c.server.as_ref())
        .filter(|s| s.is_connected());
    let selected = state
        .project
        .value
        .agents
        .get(current.agent.as_str())
        .and_then(|a| a.mcp.get(&config.id));
    let names = selected.map_or(&[][..], Vec::as_slice);
    let tools = live.map(|s| available(&config.id, s)).unwrap_or_default();
    let id = &config.id;
    if tools_page {
        let offset = offset.min(tools.len().saturating_sub(1)) / 30 * 30;
        let actions = tools
            .iter()
            .skip(offset)
            .take(30)
            .map(|tool| {
                choice(
                    format!("toggle:{id}:{}:{offset}", tool.name),
                    format!(
                        "[{}] {}",
                        if names.contains(&tool.name) {
                            "on"
                        } else {
                            "off"
                        },
                        tool.name
                    ),
                    tool.description.chars().take(120).collect::<String>(),
                )
            })
            .chain((offset > 0).then(|| {
                choice(
                    format!("tools:{id}:{}", offset.saturating_sub(30)),
                    "Previous page",
                    "",
                )
            }))
            .chain(
                (offset + 30 < tools.len())
                    .then(|| choice(format!("tools:{id}:{}", offset + 30), "Next page", "")),
            )
            .chain(std::iter::once(choice(
                format!("show:{id}"),
                "Back to server",
                "",
            )))
            .collect();
        return Ok(panel(
            current,
            format!("MCP tools · {id}"),
            format!(
                "{status} · {} available tools · {} selected for {}. Select an entry to toggle it. At most 64 MCP tools per agent.",
                tools.len(),
                names.len(),
                current.agent
            ),
            actions,
        ));
    }
    let process = live
        .and_then(|s| s.process_id())
        .map_or_else(|| "none".into(), |pid| pid.to_string());
    let body = format!(
        "Status: {status}\nProcess: {process}\nProgram: {}\nArguments (JSON): {}\nEnvironment references (child → host): {}\nWorking directory: project workspace\nConnection timeout: {} seconds\nAvailable tools: {}\nSelected for {}: {}\n\nMCP tools use mcp__{id}__<tool>. New tools are never enabled automatically after reconnect. Names unsupported by the model protocol are omitted. Status refreshes when this panel is reopened.",
        config.program,
        serde_json::to_string(&config.args).map_err(|_| "Invalid arguments")?,
        serde_json::to_string(&config.environment).map_err(|_| "Invalid environment")?,
        config.timeout_seconds,
        tools.len(),
        current.agent,
        if names.is_empty() {
            "none".into()
        } else {
            names.join(", ")
        }
    );
    let mut actions = if live.is_some() {
        vec![
            choice(
                format!("tools:{id}"),
                "Choose tools for this agent",
                "Inspect and enable individual tools",
            ),
            choice(
                format!("enable:{id}"),
                "Enable all current tools for this agent",
                "Snapshots the discovered names; maximum 64 across all servers",
            ),
            choice(
                format!("disconnect:{id}"),
                "Disconnect",
                "Stop and wait for this server's process group",
            ),
        ]
    } else {
        vec![
            choice(
                format!("connect:{id}"),
                "Connect",
                "Start the configured program and discover its tools · Esc cancels",
            ),
            choice(
                format!("edit:{id}"),
                "Edit connection",
                "Edit the program, arguments and environment references",
            ),
        ]
    };
    if live.is_none() && connection.is_some_and(|c| c.server.is_some()) {
        actions.insert(
            1,
            choice(
                format!("disconnect:{id}"),
                "Release disconnected server",
                "Stop and wait for the owned process before editing or deleting",
            ),
        );
    }
    if !names.is_empty() {
        actions.push(choice(
            format!("disable:{id}"),
            "Disable all tools for this agent",
            "Keep the connection for other configured agents",
        ));
    }
    if live.is_none() {
        actions.push(choice(
            format!("delete:{id}"),
            "Delete configuration…",
            "Requires removing selections from all agents first",
        ));
    }
    actions.push(choice(format!("show:{id}"), "Refresh status", ""));
    actions.push(choice("list", "Back to servers", ""));
    Ok(panel(current, format!("MCP · {id}"), body, actions))
}

fn field(name: &str, label: &str, value: String, help: &str) -> FormField {
    FormField {
        name: name.into(),
        label: label.into(),
        value,
        help: help.into(),
        required: true,
        kind: FieldKind::Text,
    }
}
pub(super) fn form(
    state: &mut State,
    current: &SessionConfig,
    server: Option<&Server>,
) -> Result<CommandResult, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let default = Server {
        id: String::new(),
        program: String::new(),
        args: vec![],
        environment: BTreeMap::new(),
        timeout_seconds: 30,
    };
    let config = server.unwrap_or(&default);
    let fields = vec![
        field(
            "id",
            "Server ID",
            config.id.clone(),
            "1–24 letters, digits or hyphens; cannot rename an existing server",
        ),
        field(
            "program",
            "Program",
            config.program.clone(),
            "Executable path or PATH name, for example uvx or npx; no shell parsing",
        ),
        field(
            "args",
            "Arguments · JSON array",
            serde_json::to_string(&config.args).map_err(|_| "Invalid arguments")?,
            "For example [\"-y\",\"my-mcp-server\"]; keep secrets in environment variables",
        ),
        field(
            "environment",
            "Environment references · JSON object",
            serde_json::to_string(&config.environment).map_err(|_| "Invalid environment")?,
            "For example {\"API_KEY\":\"MY_SERVICE_KEY\"}; values are host variable NAMES, not secrets",
        ),
        field(
            "timeout",
            "Connection timeout · seconds",
            config.timeout_seconds.to_string(),
            "1–120; covers handshake and discovery; Esc interrupts",
        ),
    ];
    state.pending_mcp = Some(Pending::Form {
        room: current.room.to_string(),
        form: id.clone(),
        id: server.map(|s| s.id.clone()),
    });
    Ok(CommandResult::Events(vec![Event::Form {
        conversation: current.room.as_str().into(),
        form: FormSpec {
            id,
            command: "mcp".into(),
            title: "MCP stdio server · saving does not start it".into(),
            fields,
        },
    }]))
}

pub(super) fn save(
    state: &mut State,
    current: &SessionConfig,
    form: &str,
    values: &FormValues,
) -> Result<CommandResult, String> {
    let Some(Pending::Form {
        room,
        form: expected,
        id,
    }) = &state.pending_mcp
    else {
        return Err("Reopen /mcp before saving this form".into());
    };
    if room != current.room.as_str() || expected != form {
        return Err("This MCP form is stale".into());
    }
    let value = |name: &str| values.0.get(name).map_or("", String::as_str);
    let server = Server {
        id: value("id").trim().into(),
        program: value("program").trim().into(),
        args: serde_json::from_str(value("args"))
            .map_err(|_| "Arguments must be a JSON array of strings")?,
        environment: serde_json::from_str(value("environment"))
            .map_err(|_| "Environment references must be a JSON object of string variable names")?,
        timeout_seconds: value("timeout")
            .parse()
            .map_err(|_| "Invalid connection timeout")?,
    };
    server.validate()?;
    if id.as_ref().is_some_and(|id| id != &server.id) {
        return Err("An existing server ID cannot be renamed".into());
    }
    if id.is_none() && state.mcp.value.servers.contains_key(&server.id) {
        return Err("This MCP server ID already exists".into());
    }
    let mut config = state.mcp.value.clone();
    let id = server.id.clone();
    config.servers.insert(id.clone(), server);
    config.validate()?;
    state.mcp.save(config)?;
    state.pending_mcp = None;
    Ok(CommandResult::Events(vec![Event::Notice {
        text: format!(
            "MCP server {id} saved. Open /mcp to connect and choose its tools for this agent."
        ),
    }]))
}

pub(super) fn confirm_delete(
    state: &mut State,
    current: &SessionConfig,
    server: &Server,
) -> Result<CommandResult, String> {
    if state
        .project
        .value
        .agents
        .values()
        .any(|a| a.mcp.contains_key(&server.id))
    {
        return Err("Disable this server's tools for every agent before deleting it".into());
    }
    let token = uuid::Uuid::new_v4().to_string();
    state.pending_mcp = Some(Pending::Delete {
        room: current.room.to_string(),
        token: token.clone(),
        id: server.id.clone(),
    });
    Ok(panel(
        current,
        format!("Delete MCP server {}?", server.id),
        "Removes the local connection configuration. The executable is kept.".into(),
        vec![
            choice(
                format!("confirm-delete:{}:{token}", server.id),
                "Delete configuration",
                "",
            ),
            choice("list", "Cancel", ""),
        ],
    ))
}
pub(super) fn delete(
    state: &mut State,
    current: &SessionConfig,
    id: &str,
    token: &str,
) -> Result<CommandResult, String> {
    let Some(Pending::Delete {
        room,
        token: expected,
        id: target,
    }) = &state.pending_mcp
    else {
        return Err("Reopen the delete confirmation".into());
    };
    if room != current.room.as_str() || token != expected || id != target {
        return Err("This deletion confirmation is stale".into());
    }
    state.project.reload()?;
    if state
        .project
        .value
        .agents
        .values()
        .any(|a| a.mcp.contains_key(id))
    {
        return Err("This server is still selected by an agent".into());
    }
    let mut config = state.mcp.value.clone();
    config.servers.remove(id);
    state.mcp.save(config)?;
    state.pending_mcp = None;
    Ok(CommandResult::Events(vec![Event::Notice {
        text: "MCP server configuration removed".into(),
    }]))
}
