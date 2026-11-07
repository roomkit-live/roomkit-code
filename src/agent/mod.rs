//! The tools an agent is assembled with, and their assembly; `preamble`
//! builds its system prompt and measures what its requests carry.

use roomkit::model::{ChannelId, RetryPolicy};
use roomkit_ai::{
    AiChannel, AiChannelOptions, AiTool, ImageResolver, SkillRegistry, Tool, ToolEntry,
    ToolHandler, compose_tool_handlers,
};
use roomkit_openai::{OpenAiAiProvider, OpenAiConfig};
use roomkit_tools_code::CodeOutlineTool;
use roomkit_tools_docs::{DocsBackend, DocsListTool, DocsReadTool, DocsSearchTool};
use roomkit_tools_fs::{
    DiffTool, EditTool, GlobTool, GrepTool, ListTool, PatchTool, ReadTool, Workspace, WriteTool,
};
use roomkit_tools_plans::{
    PlanCreateTool, PlanDeleteTool, PlanListTool, PlanReadTool, PlanUpdateTool,
};
use roomkit_tools_process::{
    CommandEnv, OutputLimits, OutputStore, ProcessLimits, ProcessSession, ShellLimits, ShellTool,
};
use roomkit_tools_roomkit::{
    CodeOutline, Diff, Edit, Glob, Grep, List, OutputForget, OutputRead, Patch, ProcessRead,
    ProcessStart, ProcessStatus, ProcessStop, ProcessWrite, Shell, WebFetch, WebSearch,
};
use roomkit_tools_roomkit::{
    DocsList, DocsRead, DocsSearch, PlanCreate, PlanDelete, PlanList, PlanRead, PlanUpdate,
};
use roomkit_tools_search::{ExaSearch, SearchHttpConfig, SearchLimits, WebSearchTool};
use roomkit_tools_web::{WebFetchLimits, WebFetchTool};
use std::{path::Path, sync::Arc};

use crate::{Result, profiles::model::Agent};

mod preamble;

use preamble::preamble;
pub(crate) use preamble::{Composition, Inventory};

/// The agent the application runs: an AI channel answering through an
/// OpenAI-compatible endpoint.
pub(crate) type CodeAgent = AiChannel<OpenAiAiProvider>;

/// How an agent's generation that failed before its provider handed anything
/// is tried again: the engine's default, 3 retries 1, 2 and 4 seconds apart, on
/// the failures its provider marks retryable (a lost connection, a timeout, 408,
/// 409, 429 and 5xx answers). A refusal of the request (401, 400…) and a
/// context too long for the model are not retried; an answer that began
/// streaming is never asked again (RMK-1531).
fn retry_policy() -> RetryPolicy {
    let policy = RetryPolicy::default();
    // Tests count the tries, not the waits between them.
    #[cfg(test)]
    let policy = RetryPolicy {
        base_delay_seconds: 0.01,
        ..policy
    };
    policy
}

/// How many of the room's latest events a turn reads, as the engine handed
/// every turn before it sized contexts by the channels' windows. The agent's
/// slot declares the same window, since the kit sizes the context it hands
/// the slot by it.
pub(crate) const HISTORY: usize = 50;

/// Application-scoped resources survive turn, agent and connection changes.
/// Tool adapters derive archive/process ownership from the room and agent IDs.
pub(crate) struct Resources {
    pub instructions: Arc<crate::instructions::Instructions>,
    /// Each conversation's compactions, for the agents whose memory compacts.
    pub compactions: Arc<crate::compaction::Compactions>,
    pub plans: Arc<crate::plans::Plans>,
    /// Files each conversation's agents changed or read.
    pub files: Arc<crate::files::Files>,
    /// The application's own guide, shared by every agent.
    pub docs: Arc<dyn DocsBackend>,
    pub mutations: Workspace,
    pub processes: ProcessSession,
    /// The environment of every agent's shell, as of the processes.
    pub command_env: CommandEnv,
    pub outputs: OutputStore,
    pub search: WebSearchTool,
    pub fetch: WebFetchTool,
    /// The agents a task can be handed to, which the profiles keep current.
    pub roster: Arc<crate::delegation::Roster>,
    /// What the status line says of each conversation's latest task.
    pub tasks: Arc<crate::delegation::Tasks>,
}

impl Resources {
    pub async fn new(
        root: &Path,
        state: &Path,
        global: &Path,
        inherit_env: &[String],
    ) -> Result<Self> {
        let command_env = crate::command_env::command_env(inherit_env);
        let mutations = Workspace::new(root).await?;
        // Small immediate pages leave room for the JSON envelope in the bounded
        // terminal projection. Large captures remain in separately paged archives.
        let output_limits = OutputLimits {
            max_read_bytes: 4096,
            ..OutputLimits::default()
        };
        let outputs = OutputStore::new(Some(state), output_limits).await?;
        let processes = ProcessSession::new(root)
            .await?
            .with_limits(process_limits())?
            .with_env(command_env.clone())?
            .with_output_store(outputs.clone())?;
        let search_limits = SearchLimits {
            max_output_bytes: 48 * 1024,
            ..SearchLimits::default()
        };
        let fetch_limits = WebFetchLimits {
            max_output_bytes: 8192,
            ..WebFetchLimits::default()
        };
        Ok(Self {
            instructions: crate::instructions::Instructions::new(root, global),
            compactions: Arc::new(crate::compaction::Compactions::new(
                &state.join("compactions"),
            )),
            plans: Arc::new(crate::plans::Plans::new(state.join("plans"))),
            files: crate::files::Files::new(root),
            docs: crate::docs::guide(global)?,
            mutations,
            processes,
            command_env,
            outputs,
            search: WebSearchTool::new(Arc::new(ExaSearch::new(SearchHttpConfig::default())?))
                .with_limits(search_limits)?,
            fetch: WebFetchTool::new().with_limits(fetch_limits)?,
            roster: Arc::default(),
            tasks: Arc::default(),
        })
    }

    /// Release every resource. `leave_processes` lets the running processes
    /// go instead of stopping them: an unattended launch exits while a server
    /// it started must still answer the check that follows.
    pub async fn close(&self, leave_processes: bool) -> bool {
        let plans = self.plans.close().await;
        let processes = if leave_processes {
            self.processes.detach().await
        } else {
            self.processes.close().await
        };
        let outputs = self.outputs.close().await;
        plans && processes && outputs
    }
}

/// The limits of every agent's processes: 4 KiB read pages, as for the
/// output archives. The system prompt states their maximum `timeout_ms`.
pub(crate) fn process_limits() -> ProcessLimits {
    ProcessLimits {
        max_read_bytes: 4096,
        ..ProcessLimits::default()
    }
}

/// What an agent receives beyond the application's own tools.
pub(crate) struct Extensions {
    pub skills: SkillRegistry,
    pub skill_roots: Vec<std::path::PathBuf>,
    pub mcp: Vec<crate::profiles::McpTools>,
    /// Whether every tool call must carry an intent.
    pub intent_required: bool,
    /// Rounds of tool calls one turn may run.
    pub max_tool_rounds: u32,
    /// Nobody watches the launch: the agent gets no mode tools, which wait
    /// for a person's approval, nor `ATTENDED_ONLY`, and its shell commands
    /// may run longer (`timeouts::shell_max_seconds`).
    pub unattended: bool,
    /// An unattended launch in Plan mode, whose work is a plan: it keeps the
    /// plan tools `ATTENDED_ONLY` otherwise leaves out.
    pub planning: bool,
    /// Whether the agent is told of the delegation tools the engine serves it:
    /// not when nobody watches the launch, which ends with its turn, before a
    /// delegated task could end (RMK-1532).
    pub delegation: bool,
    /// The mode policy the mode tools change.
    pub policy: crate::permissions::Policy,
    /// What resolves the conversation's image attachments for vision models.
    pub images: Arc<dyn ImageResolver>,
    /// The tokens the agent's history compacts under; none keeps its window
    /// of `HISTORY` messages.
    pub ceiling: Option<u64>,
}

pub(crate) async fn create(
    root: &std::path::Path,
    resources: &Resources,
    profile: &Agent,
    provider: OpenAiConfig,
    extensions: Extensions,
) -> Result<(CodeAgent, Inventory)> {
    profile.validate()?;
    let unattended = extensions.unattended;
    let shell = shell_limits(unattended);
    let shell_maximum = shell.max_timeout_ms;
    let policy = extensions.policy.clone();
    let tools = native_tools(root, resources, policy, shell).await?;
    let tools = selected(profile, tools, (unattended, extensions.planning));
    let native: Vec<AiTool> = tools
        .iter()
        .map(|(definition, _)| definition.clone())
        .collect();
    let Extensions {
        skills,
        skill_roots,
        mcp,
        intent_required,
        max_tool_rounds,
        delegation,
        images,
        ceiling,
        ..
    } = extensions;
    let preamble = preamble(
        profile,
        native,
        &skills,
        &skill_roots,
        &mcp,
        (intent_required, delegation),
        shell_maximum,
    );
    let mut entries: Vec<ToolEntry> = tools
        .into_iter()
        .map(|(_, tool)| ToolEntry::Tool(tool))
        .collect();
    entries.extend(
        mcp.iter()
            .flat_map(|server| server.tools.iter().cloned().map(ToolEntry::Definition)),
    );
    let options = AiChannelOptions {
        system_prompt: Some(preamble.prompt),
        tools: entries,
        tool_handler: mcp_handler(&mcp)?,
        server_instructions: mcp
            .iter()
            .map(|server| (server.label.clone(), server.instructions.clone()))
            .collect(),
        skills: Some(skills),
        tool_call_intents: true,
        tool_call_intents_required: intent_required,
        tool_timeout: Some(crate::timeouts::tool_call(unattended)),
        // A coding turn runs as long as its calls and approvals take, as
        // before: the engine's five-minute bound on a turn's tool loop would
        // end it before the calls of its next round run.
        tool_loop_timeout: None,
        max_tool_rounds,
        max_context_events: HISTORY,
        retry_policy: Some(retry_policy()),
        memory: memory((ceiling, &profile.id), &provider, resources)?,
        image_resolver: Some(images),
        // Native tools stay declared; only MCP tools wait behind find_tools.
        tool_search: Some(preamble.hides_mcp),
        tool_search_pinned: preamble.pinned,
        ..AiChannelOptions::default()
    };
    let provider = OpenAiAiProvider::new(provider)?;
    let agent = AiChannel::new(ChannelId::new(&profile.id)?, provider, options)?;
    Ok((agent, preamble.inventory))
}

/// The memory of `agent`, whose history compacts under `ceiling` tokens,
/// summarized by its own model, which also compacts its conversations
/// outside its turns; none keeps its window of `HISTORY` messages.
fn memory(
    (ceiling, agent): (Option<u64>, &str),
    provider: &OpenAiConfig,
    resources: &Resources,
) -> Result<Option<Arc<dyn roomkit_ai::memory::MemoryProvider>>> {
    let Some(ceiling) = ceiling else {
        return Ok(None);
    };
    let summarizer = OpenAiAiProvider::new(provider.clone())?;
    let compactions = resources.compactions.clone();
    let memory = Arc::new(crate::compaction::TieredMemory::new(
        summarizer,
        (ceiling, agent),
        compactions.clone(),
    ));
    compactions.register(agent, memory.clone());
    Ok(Some(memory))
}

/// The tools an unattended launch leaves out, whatever the profile selects:
/// the plans the sidebar shows and the guide to the application itself, which
/// a script or a benchmark has no use for; each request is smaller without
/// their schemas (HITL cf0fc0eb). The web tools stay: a prompt may ask to look
/// something up.
const ATTENDED_ONLY: [&str; 8] = [
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
fn selected(
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

/// The handler of the selected MCP servers' tools, each server's first in
/// the order given; none without a server.
fn mcp_handler(mcp: &[crate::profiles::McpTools]) -> Result<Option<ToolHandler>> {
    let mut handlers: Vec<ToolHandler> = mcp.iter().map(|server| server.handler.clone()).collect();
    Ok(match handlers.len() {
        0 => None,
        1 => handlers.pop(),
        _ => Some(compose_tool_handlers(handlers)?),
    })
}

/// The shell's limits. Each stream keeps its latest 16 KiB, where a command
/// reports how it ended. An unattended launch's command may run an hour, and
/// what it leaves in the background runs on: a server the check that follows
/// the launch needs, for one.
pub(crate) fn shell_limits(unattended: bool) -> ShellLimits {
    ShellLimits {
        max_output_bytes: 16 * 1024,
        max_timeout_ms: crate::timeouts::shell_max_seconds(unattended) * 1000,
        keep_background_jobs: unattended,
        ..ShellLimits::default()
    }
}

/// Every tool the application can give an agent, in inventory order, each
/// scoped to the instructions of what it reads and tracked for the files it
/// changes; its shell runs within `shell`.
async fn native_tools(
    root: &Path,
    resources: &Resources,
    policy: crate::permissions::Policy,
    shell: ShellLimits,
) -> Result<Vec<Arc<dyn Tool>>> {
    let mutations = resources.mutations.clone();
    let shell = ShellTool::new(root)
        .await?
        .with_limits(shell)?
        .with_env(resources.command_env.clone())?
        .with_output_store(resources.outputs.clone());
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
    fn an_unattended_shell_keeps_its_background_jobs_and_both_keep_their_end() {
        let terminal = shell_limits(false);
        let unattended = shell_limits(true);
        assert_eq!(terminal.max_output_bytes, 16 * 1024);
        assert_eq!(unattended.max_output_bytes, 16 * 1024);
        assert!(!terminal.keep_background_jobs);
        assert!(unattended.keep_background_jobs);
        assert_eq!(terminal.max_timeout_ms, 120_000);
        assert_eq!(unattended.max_timeout_ms, 3_600_000);
    }
}
