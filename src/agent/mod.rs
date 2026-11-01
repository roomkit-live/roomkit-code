//! The tools an agent is assembled with, and their assembly; `preamble`
//! builds its system prompt and measures what its requests carry.

use roomkit::model::ChannelId;
use roomkit_ai::{SkillRegistry, ToolDefinition, Toolset};
use roomkit_openai::{OpenAiAgent, OpenAiConfig};
use roomkit_tools_code::CodeOutlineTool;
use roomkit_tools_docs::{DocsBackend, DocsListTool, DocsReadTool, DocsSearchTool};
use roomkit_tools_fs::{
    DiffTool, EditTool, GlobTool, GrepTool, ListTool, MutationWorkspace, PatchTool, ReadTool,
    WriteTool,
};
use roomkit_tools_plans::{
    PlanCreateTool, PlanDeleteTool, PlanListTool, PlanReadTool, PlanUpdateTool,
};
use roomkit_tools_process::{
    CommandEnv, OutputLimits, OutputStore, ProcessLimits, ProcessManager, ShellLimits, ShellTool,
};
use roomkit_tools_roomkit::{
    CodeOutlineToolset, DiffToolset, EditToolset, GlobToolset, GrepToolset, ListToolset,
    OutputForgetToolset, OutputReadToolset, PatchToolset, ProcessReadToolset, ProcessStartToolset,
    ProcessStatusToolset, ProcessStopToolset, ProcessWriteToolset, ReadToolset, ShellToolset,
    WebFetchToolset, WebSearchToolset, WriteToolset,
};
use roomkit_tools_roomkit::{
    DocsListToolset, DocsReadToolset, DocsSearchToolset, PlanCreateToolset, PlanDeleteToolset,
    PlanListToolset, PlanReadToolset, PlanUpdateToolset,
};
use roomkit_tools_search::{ExaSearch, SearchHttpConfig, SearchLimits, WebSearchTool};
use roomkit_tools_web::{WebFetchLimits, WebFetchTool};
use std::{path::Path, sync::Arc};

use crate::{Result, profiles::model::Agent};

mod preamble;

use preamble::preamble;
pub(crate) use preamble::{Composition, Inventory};

/// Application-scoped resources survive turn, agent and connection changes.
/// Tool adapters derive archive/process ownership from the room and agent IDs.
pub(crate) struct Resources {
    pub instructions: Arc<crate::instructions::Instructions>,
    pub plans: Arc<crate::plans::Plans>,
    /// Files each conversation's agents changed or read.
    pub files: Arc<crate::files::Files>,
    /// The application's own guide, shared by every agent.
    pub docs: Arc<dyn DocsBackend>,
    pub mutations: MutationWorkspace,
    pub processes: ProcessManager,
    /// The environment of every agent's shell, as of the processes.
    pub command_env: CommandEnv,
    pub outputs: OutputStore,
    pub search: WebSearchTool,
    pub fetch: WebFetchTool,
}

impl Resources {
    pub async fn new(
        root: &Path,
        state: &Path,
        global: &Path,
        inherit_env: &[String],
    ) -> Result<Self> {
        let command_env = crate::command_env::command_env(inherit_env);
        let mutations = MutationWorkspace::new(root).await?;
        // Small immediate pages leave room for the JSON envelope in the bounded
        // terminal projection. Large captures remain in separately paged archives.
        let mut output_limits = OutputLimits::default();
        output_limits.max_read_bytes = 4096;
        let outputs = OutputStore::new(state, output_limits).await?;
        let mut process_limits = ProcessLimits::default();
        process_limits.max_read_bytes = 4096;
        let processes = ProcessManager::new(root)
            .await?
            .with_limits(process_limits)?
            .with_env(command_env.clone())?
            .with_output_store(outputs.clone())?;
        let mut search_limits = SearchLimits::default();
        search_limits.max_output_bytes = 48 * 1024;
        let mut fetch_limits = WebFetchLimits::default();
        fetch_limits.max_output_bytes = 8192;
        Ok(Self {
            instructions: crate::instructions::Instructions::new(root, global),
            plans: Arc::new(crate::plans::Plans::new(state.join("plans"))),
            files: crate::files::Files::new(root),
            docs: crate::docs::guide(global)?,
            mutations,
            processes,
            command_env,
            outputs,
            search: WebSearchTool::new(Arc::new(ExaSearch::new(
                None,
                SearchHttpConfig::default(),
            )?))
            .with_limits(search_limits)?,
            fetch: WebFetchTool::new().with_limits(fetch_limits)?,
        })
    }

    pub async fn close(&self) -> bool {
        let plans = self.plans.close().await;
        let processes = self.processes.close().await;
        let outputs = self.outputs.close().await;
        plans && processes && outputs
    }
}

/// Every toolset is named once: its tools join the inventory, and it is
/// registered when the profile selects one of them. The toolsets' types
/// differ, and `Toolset` is not object-safe, hence a macro.
macro_rules! assemble {
    ($id:expr, $profile:expr, $provider:expr, $extensions:expr, $instructions:expr, $files:expr; $($binding:ident = $tool:expr),+ $(,)?) => {{
        $(let $binding = $tool;)+
        // The mode tools come with plan writing: leaving Plan approves a plan.
        let plans = $profile.tools.iter().any(|tool| tool == "plan_create");
        let selected = |tool: &ToolDefinition| {
            $profile.tools.contains(&tool.name)
                || (plans && crate::permissions::MODE_TOOLS.contains(&tool.name.as_str()))
        };
        let native: Vec<ToolDefinition> = [$($binding.tools()),+].concat().into_iter().filter(selected).collect();
        let Extensions { skills, skill_roots, mcp, intent_required, .. } = $extensions;
        let preamble = preamble($profile, native, &skills, &skill_roots, &mcp, intent_required);
        let mut agent = OpenAiAgent::new($id, $provider.with_system_prompt(preamble.prompt))?
            .with_tool_call_intents(true)
            .with_tool_call_intents_required(intent_required)
            .with_skills(skills)?;
        $(if $binding.tools().iter().any(selected) {
            agent = agent.with_tools($files.wrap($instructions.wrap($binding)))?;
        })+
        for (label, server) in mcp {
            agent = agent.with_server_instructions(&label, server.instructions())?.with_tools(server)?;
        }
        let agent = agent.with_tool_timeout(Some(crate::timeouts::TOOL_CALL));
        Ok((agent, preamble.inventory))
    }};
}

/// What an agent receives beyond the application's own tools.
pub(crate) struct Extensions {
    pub skills: SkillRegistry,
    pub skill_roots: Vec<std::path::PathBuf>,
    pub mcp: Vec<(String, roomkit_mcp::McpServer)>,
    /// Whether every tool call must carry an intent.
    pub intent_required: bool,
    /// The mode policy the mode tools change.
    pub policy: crate::permissions::Policy,
}

pub(crate) async fn create(
    root: &std::path::Path,
    resources: &Resources,
    profile: &Agent,
    provider: OpenAiConfig,
    extensions: Extensions,
) -> Result<(OpenAiAgent, Inventory)> {
    profile.validate()?;
    let mutations = resources.mutations.clone();
    let mut shell_limits = ShellLimits::default();
    shell_limits.max_output_bytes = 8192;
    shell_limits.max_timeout_ms = crate::timeouts::SHELL_MAX_SECONDS * 1000;
    assemble!(ChannelId::new(&profile.id)?, profile, provider, extensions, resources.instructions, resources.files;
        outline = CodeOutlineToolset::new(CodeOutlineTool::new(root).await?),
        list = ListToolset::new(ListTool::new(root).await?),
        glob = GlobToolset::new(GlobTool::new(root).await?),
        grep = GrepToolset::new(GrepTool::new(root).await?),
        read = ReadToolset::new(ReadTool::new(root).await?),
        diff = DiffToolset::new(DiffTool::new(root).await?),
        write = WriteToolset::new(WriteTool::from_workspace(mutations.clone())),
        edit = EditToolset::new(EditTool::from_workspace(mutations.clone())),
        patch = PatchToolset::new(PatchTool::from_workspace(mutations)),
        shell = ShellToolset::new(ShellTool::new(root).await?.with_limits(shell_limits)?.with_env(resources.command_env.clone())?.with_output_store(resources.outputs.clone())),
        fetch = WebFetchToolset::new(resources.fetch.clone()),
        search = WebSearchToolset::new(resources.search.clone()),
        start = ProcessStartToolset::new(resources.processes.clone()),
        status = ProcessStatusToolset::new(resources.processes.clone()),
        process_read = ProcessReadToolset::new(resources.processes.clone()),
        process_write = ProcessWriteToolset::new(resources.processes.clone()),
        stop = ProcessStopToolset::new(resources.processes.clone()),
        output_read = OutputReadToolset::new(resources.outputs.clone()),
        forget = OutputForgetToolset::new(resources.outputs.clone()),
        plan_create = PlanCreateToolset::new(PlanCreateTool::new(resources.plans.clone())),
        plan_read = PlanReadToolset::new(PlanReadTool::new(resources.plans.clone())),
        plan_list = PlanListToolset::new(PlanListTool::new(resources.plans.clone())),
        plan_update = PlanUpdateToolset::new(PlanUpdateTool::new(resources.plans.clone())),
        plan_delete = PlanDeleteToolset::new(PlanDeleteTool::new(resources.plans.clone())),
        docs_list = DocsListToolset::new(DocsListTool::new(resources.docs.clone())),
        docs_search = DocsSearchToolset::new(DocsSearchTool::new(resources.docs.clone())),
        docs_read = DocsReadToolset::new(DocsReadTool::new(resources.docs.clone())),
        modes = crate::permissions::ModeTools::new(extensions.policy.clone(), resources.plans.clone()),
    )
}
