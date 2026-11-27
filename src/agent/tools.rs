//! The tools an agent is given: those its profile selects, the engine's own,
//! the pool's, and the limits of the shell.
use super::*;

/// The tools an unattended launch leaves out, whatever the profile selects:
/// the plans the sidebar shows and the guide to the application itself, which
/// a script or a benchmark has no use for; each request is smaller without
/// their schemas (HITL cf0fc0eb). The web tools stay: a prompt may ask to look
/// something up.
pub(super) const ATTENDED_ONLY: [&str; 8] = [
    "plan_create",
    "plan_read",
    "plan_list",
    "plan_update",
    "plan_delete",
    "docs_list",
    "docs_search",
    "docs_read",
];

/// The tools of `tools` the profile selects, with their definitions, in
/// inventory order: every tool is named once, it joins the inventory, and
/// the agent serves it. The mode tools come with plan writing: leaving Plan
/// approves a plan. An `unattended` launch gets none of them, since nobody
/// could approve leaving Plan and the agent would stay unable to act, nor
/// `ATTENDED_ONLY`, but for the plan tools when it is `planning`.
pub(super) fn selected(
    profile: &Agent,
    tools: Vec<Arc<dyn Tool>>,
    (unattended, planning): (bool, bool),
) -> Vec<(AiTool, Arc<dyn Tool>)> {
    let left_out = |name: &str| {
        unattended && ATTENDED_ONLY.contains(&name) && !(planning && name.starts_with("plan_"))
    };
    let chosen = |name: &str| profile.tools.iter().any(|tool| tool == name) && !left_out(name);
    let plans = !unattended && chosen("plan_create");
    let selected = |tool: &AiTool| {
        chosen(&tool.name)
            || (plans && crate::permissions::MODE_TOOLS.contains(&tool.name.as_str()))
    };
    tools
        .into_iter()
        .map(|tool| (tool.definition(), tool))
        .filter(|(definition, _)| selected(definition))
        .collect()
}

/// A profile opting into the tool gets its index under the same allowance.
pub(super) fn add_memory(
    tools: &mut Vec<Arc<dyn Tool>>,
    profile: &Agent,
    extensions: &Extensions,
) -> Option<crate::project_memory::View> {
    profile.tools.iter().any(|name| name == "memory").then(|| {
        let view = crate::project_memory::View::new(
            extensions.project_memory.clone(),
            extensions.ceiling,
            profile.max_output_tokens,
        );
        tools.push(Arc::new(view.tool(extensions.history.clone())));
        view
    })
}

/// Owners and workers see the inventory. Only an opted-in worker profile
/// exposes the fixed Cargo command.
pub(super) fn add_pool_tools(
    tools: &mut Vec<(AiTool, Arc<dyn Tool>)>,
    inventory: crate::remote::pool::Inventory,
    channel: &str,
    profile: &Agent,
    root: &Path,
    resources: &Resources,
) {
    use crate::remote::pool::{CARGO_TEST, CargoTest, PeerTool};
    if channel == crate::names::REMOTE_AGENT {
        let reply = crate::messages::WorkerReply;
        tools.push((reply.definition(), Arc::new(reply)));
        for tool in [PeerTool::Ask, PeerTool::Answer] {
            tools.push((tool.definition(), Arc::new(tool)));
        }
    }
    if channel == crate::names::REMOTE_AGENT && profile.tools.iter().any(|name| name == CARGO_TEST)
    {
        let runner = CargoTest {
            workspace: root.into(),
            shell: Shell::new(resources.shell.clone()),
            env: resources.command_env.clone(),
            outputs: resources.outputs.clone(),
        };
        tools.push((runner.definition(), Arc::new(runner)));
    }
    tools.push((inventory.definition(), Arc::new(inventory)));
}

/// The handler of the selected MCP servers' tools, each server's first in
/// the order given; none without a server.
pub(super) fn mcp_handler(mcp: &[crate::profiles::McpTools]) -> Result<Option<ToolHandler>> {
    let mut handlers: Vec<ToolHandler> = mcp.iter().map(|server| server.handler.clone()).collect();
    Ok(match handlers.len() {
        0 => None,
        1 => handlers.pop(),
        _ => Some(compose_tool_handlers(handlers)?),
    })
}

/// The tools the engine serves the agent beside those it selects: the
/// delegation tools, and `ask_user` in an attended launch.
pub(super) fn engine_tools(
    delegation: bool,
    asking: Option<&roomkit_ai::HumanInputToolHandler>,
) -> Vec<AiTool> {
    let mut tools = if delegation {
        crate::delegation::definitions()
    } else {
        Vec::new()
    };
    tools.extend(
        asking
            .map(roomkit_ai::HumanInputToolHandler::tools)
            .unwrap_or_default(),
    );
    tools
}

/// Whether a tool's calls ask the model for an intention: those whose
/// arguments do not say what they are for. The terminal reads the others from
/// their arguments (`tool_views::labels`).
pub(crate) fn asks_intent(name: &str) -> bool {
    matches!(name, "shell" | "process_start" | "delegate_task") || name.starts_with("mcp__")
}

/// Bind native implementations and the definitions served by the MCP tool handler.
pub(super) fn tool_entries(
    native: Vec<(AiTool, Arc<dyn Tool>)>,
    mcp: &[crate::profiles::McpTools],
) -> Vec<ToolEntry> {
    let mut entries: Vec<_> = native
        .into_iter()
        .map(|(_, tool)| ToolEntry::Tool(tool))
        .collect();
    entries.extend(
        mcp.iter()
            .flat_map(|server| server.tools.iter().cloned().map(ToolEntry::Definition)),
    );
    entries
}

/// The tools of an agent with `mcp` that ask the model for an intention.
pub(super) fn intent_tools(mcp: &[crate::profiles::McpTools]) -> Vec<String> {
    ["shell", "process_start", "delegate_task"]
        .into_iter()
        .map(str::to_owned)
        .chain(
            mcp.iter()
                .flat_map(|server| server.tools.iter().map(|tool| tool.name.clone())),
        )
        .filter(|name| asks_intent(name))
        .collect()
}

/// The shell's limits. Each stream keeps its latest 16 KiB, where a command
/// reports how it ended. An unattended launch's command may run an hour.
/// What a command leaves in the background runs on, followed until it ends:
/// a server the agent then queries, for one. Closing the application stops
/// it, unless the launch is unattended: a check that follows the launch may
/// need it. In the terminal, a command still running at its timeout, a build
/// whose length nobody can foresee, moves to the background as a process,
/// whose end the agent is told; an unattended launch, told nothing, stops it.
pub(crate) fn shell_limits(unattended: bool) -> ShellLimits {
    ShellLimits {
        max_output_bytes: 16 * 1024,
        max_timeout_ms: crate::timeouts::shell_max_seconds(unattended) * 1000,
        keep_background_jobs: true,
        background_on_timeout: !unattended,
        ..ShellLimits::default()
    }
}

/// Every tool the application can give an agent, in inventory order, each
/// scoped to the instructions of what it reads and tracked for the files it
/// changes; its shell is the application's.
pub(super) async fn native_tools(
    root: &Path,
    resources: &Resources,
    policy: crate::permissions::Policy,
) -> Result<Vec<Arc<dyn Tool>>> {
    let mutations = resources.mutations.clone();
    let shell = resources.shell.clone();
    let processes = &resources.processes;
    let plans = &resources.plans;
    let docs = &resources.docs;
    let modes = crate::permissions::ModeTools::new(policy, plans.clone());
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(CodeOutline::new(CodeOutlineTool::new(root).await?)),
        Arc::new(List::new(ListTool::new(root).await?)),
        Arc::new(Glob::new(GlobTool::new(root).await?)),
        Arc::new(Grep::new(GrepTool::new(root).await?)),
        Arc::new(roomkit_tools_roomkit::Read::new(ReadTool::new(root).await?)),
        Arc::new(Diff::new(DiffTool::new(root).await?)),
        Arc::new(roomkit_tools_roomkit::Write::new(
            WriteTool::from_workspace(mutations.clone()),
        )),
        Arc::new(Edit::new(EditTool::from_workspace(mutations.clone()))),
        Arc::new(Patch::new(PatchTool::from_workspace(mutations))),
        Arc::new(Shell::new(shell)),
        Arc::new(WebFetch::new(resources.fetch.clone())),
        Arc::new(WebSearch::new(resources.search.clone())),
        Arc::new(ProcessStart::new(processes.clone())),
        Arc::new(ProcessStatus::new(processes.clone())),
        Arc::new(ProcessRead::new(processes.clone())),
        Arc::new(ProcessWrite::new(processes.clone())),
        Arc::new(ProcessStop::new(processes.clone())),
        Arc::new(OutputRead::new(resources.outputs.clone())),
        Arc::new(OutputForget::new(resources.outputs.clone())),
        Arc::new(PlanCreate::new(PlanCreateTool::new(plans.clone()))),
        Arc::new(PlanRead::new(PlanReadTool::new(plans.clone()))),
        Arc::new(PlanList::new(PlanListTool::new(plans.clone()))),
        Arc::new(PlanUpdate::new(PlanUpdateTool::new(plans.clone()))),
        Arc::new(PlanDelete::new(PlanDeleteTool::new(plans.clone()))),
        Arc::new(DocsList::new(DocsListTool::new(docs.clone()))),
        Arc::new(DocsSearch::new(DocsSearchTool::new(docs.clone()))),
        Arc::new(DocsRead::new(DocsReadTool::new(docs.clone()))),
    ];
    tools.extend(modes.tools());
    Ok(tools
        .into_iter()
        .map(|tool| resources.files.wrap(resources.instructions.wrap(tool)))
        .collect())
}

#[cfg(test)]
mod shell_tests {
    use super::shell_limits;

    #[test]
    fn both_shells_keep_their_background_jobs_and_their_end() {
        let terminal = shell_limits(false);
        let unattended = shell_limits(true);
        assert_eq!(terminal.max_output_bytes, 16 * 1024);
        assert_eq!(unattended.max_output_bytes, 16 * 1024);
        assert!(terminal.keep_background_jobs);
        assert!(unattended.keep_background_jobs);
        assert_eq!(terminal.max_timeout_ms, 120_000);
        assert_eq!(unattended.max_timeout_ms, 3_600_000);
        // Only an agent told of its jobs' end has a timed-out command moved.
        assert!(terminal.background_on_timeout);
        assert!(!unattended.background_on_timeout);
    }
}
