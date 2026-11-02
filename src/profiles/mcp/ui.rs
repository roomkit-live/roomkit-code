use super::super::ui::choice;
use super::super::*;
use super::{
    Runtime,
    config::{Server, Transport},
};
use roomkit_mcp::McpToolProvider;
use roomkit_tui::protocol::{Choice, Event, FieldKind, FormField, FormSpec, FormValues, PanelSpec};
use roomkit_tui_engine::CommandResult;

pub(in crate::profiles) enum Pending {
    Form {
        room: String,
        form: String,
        id: Option<String>,
        http: bool,
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
        panel: PanelSpec::new(uuid::Uuid::new_v4().to_string(), "mcp", title, body)
            .with_actions(actions)
            .with_dismiss("close"),
    }])
}
pub(super) async fn list(
    state: &State,
    current: &SessionConfig,
    runtime: &Runtime,
) -> CommandResult {
    let connections = runtime.connections.lock().await;
    let agent = state.agent(current.agent.as_str());
    let mut actions = vec![
        choice(
            "add",
            "Add a stdio server",
            "Save to connect and enable tools for the current agent",
        ),
        choice(
            "add-http",
            "Add an HTTP server",
            "Configure a Streamable HTTP endpoint and optional credentials",
        ),
    ];
    for id in state.mcp.value.servers.keys() {
        let status = connections.get(id).map_or("Disconnected", |c| c.status());
        let selected = agent
            .as_ref()
            .and_then(|a| a.mcp.get(id))
            .map_or(0, Vec::len);
        actions.push(choice(
            format!("show:{id}"),
            id,
            format!("{status} · {selected} tools selected for this agent"),
        ));
    }
    panel(current, format!("MCP · agent {}", current.agent),
        "Connect local stdio or remote Streamable HTTP servers. Local servers run with your user permissions, in this workspace. Saving connects the server and enables its tools for the current agent. Existing selections, including disabled tools, are preserved. Connections last until disconnect or application exit. Ask approves every MCP tool call; Plan blocks MCP calls; Yolo runs selected tools without asking. Servers and tools are independent of the agent's model provider.".into(), actions)
}

pub(super) fn available(id: &str, server: &McpToolProvider) -> Vec<AiTool> {
    server
        .get_tools()
        .unwrap_or_default()
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
        .filter(|s| s.connected());
    let agent = state.agent(current.agent.as_str());
    let selected = agent.as_ref().and_then(|a| a.mcp.get(&config.id));
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
    let connection_details = match &config.transport {
        Transport::Stdio {
            program,
            args,
            environment,
        } => format!(
            "Transport: stdio\nProcess: {process}\nProgram: {program}\nArguments (JSON): {}\nEnvironment references (child → host): {}\nWorking directory: project workspace",
            serde_json::to_string(args).map_err(|_| "Invalid arguments")?,
            serde_json::to_string(environment).map_err(|_| "Invalid environment")?
        ),
        Transport::Http {
            url,
            bearer_env,
            headers,
        } => format!(
            "Transport: Streamable HTTP\nEndpoint: {url}\nBearer token environment: {}\nHeader references (header → host variable): {}",
            bearer_env.as_deref().unwrap_or("none"),
            serde_json::to_string(headers).map_err(|_| "Invalid headers")?
        ),
    };
    let body = format!(
        "Status: {status}\n{connection_details}\nConnection timeout: {} seconds\nAvailable tools: {}\nSelected for {}: {}\n\nMCP tools use mcp__{id}__<tool>. Existing tool selections are preserved on save and reconnect. Names unsupported by the model protocol are omitted. Status refreshes when this panel is reopened.",
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
                "Close the connection and release its process or HTTP session",
            ),
        ]
    } else {
        vec![choice(
            format!("connect:{id}"),
            "Connect",
            "Connect and discover available tools · Esc cancels",
        )]
    };
    actions.push(choice(
        format!("edit:{id}"),
        "Edit connection",
        "Save to reconnect; existing tool selections are preserved",
    ));
    if live.is_none() && connection.is_some_and(|c| c.server.is_some()) {
        actions.insert(
            1,
            choice(
                format!("disconnect:{id}"),
                "Release disconnected server",
                "Release connection resources before editing or deleting",
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
    FormField::new(name, label, FieldKind::Text)
        .with_help(help)
        .with_value(value)
        .required()
}
pub(super) fn form(
    state: &mut State,
    current: &SessionConfig,
    server: Option<&Server>,
    http: bool,
) -> Result<CommandResult, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let default = Server {
        id: String::new(),
        transport: Transport::empty(http),
        timeout_seconds: 30,
    };
    let config = server.unwrap_or(&default);
    let mut fields = vec![field(
        "id",
        "Server ID",
        config.id.clone(),
        "1–24 letters, digits or hyphens; cannot rename an existing server",
    )];
    match &config.transport {
        Transport::Stdio { program, args, environment } => fields.extend([
            field(
                "program", "Program", program.clone(),
                "Executable path or PATH name, for example uvx or npx; no shell parsing",
            ),
            field(
                "args", "Arguments · JSON array",
                serde_json::to_string(args).map_err(|_| "Invalid arguments")?,
                "For example [\"-y\",\"my-mcp-server\"]; keep secrets in environment variables",
            ),
            field(
                "environment", "Environment references · JSON object",
                serde_json::to_string(environment).map_err(|_| "Invalid environment")?,
                "For example {\"API_KEY\":\"MY_SERVICE_KEY\"}; values are host variable NAMES, not secrets",
            ),
        ]),
        Transport::Http { url, bearer_env, headers } => {
            fields.push(field(
                "url", "Endpoint URL", url.clone(),
                "Full Streamable HTTP endpoint, for example https://mcp.example.com/mcp; no URL credentials",
            ));
            let mut bearer = field(
                "bearer_env", "Bearer token · environment variable", bearer_env.clone().unwrap_or_default(),
                "Optional variable NAME, for example MY_MCP_TOKEN; the app adds Bearer automatically",
            );
            bearer.required = false;
            fields.push(bearer);
            fields.push(field(
                "headers", "Header references · JSON object",
                serde_json::to_string(headers).map_err(|_| "Invalid headers")?,
                "Optional headers, e.g. {\"X-API-Key\":\"MY_MCP_KEY\"}; use {} for none; values are variable NAMES",
            ));
        }
    }
    fields.push(field(
        "timeout",
        "Connection timeout · seconds",
        config.timeout_seconds.to_string(),
        "1–120; covers handshake and discovery; Esc interrupts",
    ));
    state.pending_mcp = Some(Pending::Form {
        room: current.room.to_string(),
        form: id.clone(),
        id: server.map(|s| s.id.clone()),
        http,
    });
    Ok(CommandResult::Events(vec![Event::Form {
        conversation: current.room.as_str().into(),
        form: FormSpec::new(
            id,
            "mcp",
            if http {
                "MCP HTTP server · save to connect and enable tools"
            } else {
                "MCP stdio server · save to connect and enable tools"
            },
            fields,
        ),
    }]))
}

pub(super) fn save(
    state: &mut State,
    current: &SessionConfig,
    form: &str,
    values: &FormValues,
    workspace: &std::path::Path,
) -> Result<(Server, McpToolProvider), String> {
    let Some(Pending::Form {
        room,
        form: expected,
        id,
        http,
    }) = &state.pending_mcp
    else {
        return Err("Reopen /mcp before saving this form".into());
    };
    if room != current.room.as_str() || expected != form {
        return Err("This MCP form is stale".into());
    }
    let value = |name: &str| values.get(name).unwrap_or("");
    let server = Server {
        id: value("id").trim().into(),
        transport: if *http {
            Transport::Http {
                url: value("url").trim().into(),
                bearer_env: (!value("bearer_env").trim().is_empty())
                    .then(|| value("bearer_env").trim().into()),
                headers: serde_json::from_str(value("headers")).map_err(
                    |_| "Header references must be a JSON object of string variable names",
                )?,
            }
        } else {
            Transport::Stdio {
                program: value("program").trim().into(),
                args: serde_json::from_str(value("args"))
                    .map_err(|_| "Arguments must be a JSON array of strings")?,
                environment: serde_json::from_str(value("environment")).map_err(
                    |_| "Environment references must be a JSON object of string variable names",
                )?,
            }
        },
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
    // Resolve credentials before saving: invalid fields leave both the form and
    // any live connection intact. Network work starts after command admission.
    let connector = server.connection(workspace)?;
    let mut config = state.mcp.value.clone();
    config.servers.insert(server.id.clone(), server.clone());
    config.validate()?;
    state.mcp.save(config)?;
    state.pending_mcp = None;
    Ok((server, connector))
}

pub(super) fn confirm_delete(
    state: &mut State,
    current: &SessionConfig,
    server: &Server,
) -> Result<CommandResult, String> {
    if state
        .agents()
        .iter()
        .any(|(a, _)| a.mcp.get(&server.id).is_some_and(|names| !names.is_empty()))
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
        "Removes the saved connection configuration. The server itself is kept.".into(),
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
    state.reload_agents()?;
    if state
        .agents()
        .iter()
        .any(|(a, _)| a.mcp.get(id).is_some_and(|names| !names.is_empty()))
    {
        return Err("This server is still selected by an agent".into());
    }
    // Empty entries remember an explicit disabled selection while a server
    // exists. Remove those markers along with its configuration, for project
    // agents and for this project's choices of global agents.
    let mut project = state.project.value.clone();
    let mut changed = false;
    for agent in project.agents.values_mut() {
        changed |= agent.mcp.remove(id).is_some();
    }
    for extension in project.extensions.values_mut() {
        changed |= extension.mcp.remove(id).is_some();
    }
    if changed {
        state.project.save(project)?;
    }
    let mut config = state.mcp.value.clone();
    config.servers.remove(id);
    state.mcp.save(config)?;
    state.pending_mcp = None;
    Ok(CommandResult::Events(vec![Event::Notice {
        conversation: current.room.as_str().into(),
        text: "MCP server configuration removed".into(),
    }]))
}
