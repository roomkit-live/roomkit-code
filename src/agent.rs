//! The product's development instructions and selected tools.

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
    OutputLimits, OutputStore, ProcessLimits, ProcessManager, ShellLimits, ShellTool,
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

const INSTRUCTIONS: &str = include_str!("coding-agent.txt");
const ACTIVITY_INSTRUCTIONS: &str = r#"For every tool call, include a short description of your next action in _roomkit_intent, in the user's language. Put action announcements there, not in a separate message. Keep messages for findings, questions, explanations and the final answer.
Use this only for the field described as "Shown to the user; never passed to the tool"; respect any tool-owned parameter of the same name. Never add an absent field."#;

/// What every request of an agent carries before its messages, in characters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Composition {
    /// The application's instructions, the agent's own and the tool inventory.
    pub system: usize,
    /// Native tool schemas.
    pub tools: usize,
    /// The skills manifest, its discovery context and the skill tools.
    pub skills: usize,
    /// Per MCP server, by label: its instructions and tool schemas.
    pub mcp: Vec<(String, usize)>,
}

/// The tools an agent exposes, and what every one of its requests carries.
#[derive(Clone, Debug, Default)]
pub(crate) struct Inventory {
    pub tools: Vec<ToolDefinition>,
    pub composition: Composition,
}

/// Bytes the engine adds to the system prompt, with their separating blank
/// lines: roomkit-ai's `skills::texts::PREAMBLE` and the heading and
/// introduction of its `server_instructions` block, which are not public.
/// `application::tests::breakdown` compares them with a request.
const SKILLS_PREAMBLE: usize = 309 + 4;
const SERVERS_BLOCK: usize = 345 + 6;
/// The engine keeps at most this many characters of a server's instructions.
const SERVER_INSTRUCTIONS_MAX: usize = 8192;

/// Tools as an OpenAI-compatible request sends them: with the reserved intent
/// property the engine adds, required when the application requires it, each
/// wrapped as a function.
fn schema_chars(tools: &[ToolDefinition], intent_required: bool) -> usize {
    tools
        .iter()
        .map(|tool| {
            let parameters = roomkit::tool_intent::presented_parameters_with_requirement(
                &tool.parameters,
                intent_required,
            )
            .unwrap_or_else(|| tool.parameters.clone());
            serde_json::json!({
                "type": "function",
                "function": {"name": tool.name, "description": tool.description, "parameters": parameters}
            })
            .to_string()
            .len()
        })
        .sum()
}

/// A server's instructions as the engine keeps them, trimmed and cut at
/// `SERVER_INSTRUCTIONS_MAX` characters, fenced under a heading naming it and
/// apart from the next server's; nothing for a blank text. In bytes.
fn instruction_chars(label: &str, instructions: Option<&str>) -> usize {
    let text = instructions.map_or("", str::trim);
    if text.is_empty() {
        return 0;
    }
    let heading = "## MCP server: \n".len() + label.len();
    let fence = "<server_instructions>\n".len() + "\n</server_instructions>".len();
    let kept = text
        .char_indices()
        .nth(SERVER_INSTRUCTIONS_MAX)
        .map_or(text.len(), |(end, _)| end);
    heading + fence + "\n\n".len() + kept
}

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
    pub outputs: OutputStore,
    pub search: WebSearchTool,
    pub fetch: WebFetchTool,
}

impl Resources {
    pub async fn new(root: &Path, state: &Path, global: &Path) -> Result<Self> {
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

/// Inventory and registration are derived from the same selected toolsets.
macro_rules! assemble {
    ($id:expr, $profile:expr, $provider:expr, $skills:expr, $skill_roots:expr, $mcp:expr, $instructions:expr, $files:expr, $intent_required:expr; $($binding:ident = $tool:expr),+ $(,)?) => {{
        $(let $binding = $tool;)+
        let native = [$($binding.tools()),+].concat().into_iter().filter(|tool| $profile.tools.contains(&tool.name)).collect::<Vec<_>>();
        let mut tools = native.clone();
        tools.extend($skills.tool_definitions());
        for (_, server) in &$mcp { tools.extend(server.tools()); }
        let inventory = tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>().join(", ");
        let prompt = format!("{INSTRUCTIONS}\nCurrent agent: {} (ID: {}). Answer as this selected participant. Other participants may have different tools and providers.\nAgent instructions:\n{}\nCurrent capability inventory (provided by the application for this run):\n{inventory}\nOnly tools in this inventory are available to you. Apply tool-specific guidance only when that tool is present.\n", $profile.label, $profile.id, $profile.instructions);
        let context = skill_context(&$skills, $skill_roots);
        let preamble = if $skills.is_empty() { 0 } else { SKILLS_PREAMBLE };
        let mut composition = Composition {
            system: prompt.len() + ACTIVITY_INSTRUCTIONS.len(),
            tools: schema_chars(&native, $intent_required),
            skills: context.len() + preamble + $skills.to_prompt_xml().len() + schema_chars(&$skills.tool_definitions(), $intent_required),
            mcp: Vec::new(),
        };
        if $mcp.iter().any(|(label, server)| instruction_chars(label, server.instructions()) > 0) {
            composition.system += SERVERS_BLOCK;
        }
        let prompt = format!("{prompt}\n{context}\n{ACTIVITY_INSTRUCTIONS}");
        let mut agent = OpenAiAgent::new($id, $provider.with_system_prompt(prompt))?.with_tool_call_intents(true).with_tool_call_intents_required($intent_required).with_skills($skills)?;
        $(if $binding.tools().iter().any(|tool| $profile.tools.contains(&tool.name)) { agent = agent.with_tools($files.wrap($instructions.wrap($binding)))?; })+
        for (label, server) in $mcp {
            let size = instruction_chars(&label, server.instructions()) + schema_chars(&server.tools(), $intent_required);
            composition.mcp.push((label.clone(), size));
            agent = agent.with_server_instructions(&label, server.instructions())?.with_tools(server)?;
        }
        let agent = agent.with_tool_timeout(Some(std::time::Duration::from_secs(130)));
        Ok((agent, Inventory { tools, composition }))
    }};
}

/// Installation facts remain available before the first skill is enabled.
/// Only the selected packages' names and paths accompany the engine's metadata;
/// their instructions still load through activate_skill.
fn skill_context(skills: &SkillRegistry, roots: &[std::path::PathBuf]) -> String {
    let roots = roots
        .iter()
        .map(|path| format!("- {}", serde_json::json!(path.to_string_lossy())))
        .collect::<Vec<_>>()
        .join("\n");
    let available = if skills.is_empty() {
        "No skills are currently available to this agent. Creation with available file tools and user configuration through /skills are still supported.".into()
    } else {
        let locations = skills
            .skills()
            .iter()
            .map(|skill| {
                format!(
                    "- {}: {}",
                    skill.name(),
                    serde_json::json!(skill.path().to_string_lossy())
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Available skill package directories (resolve relative paths in their instructions from these directories):\n{locations}"
        )
    };
    format!(
        "Skill discovery roots for this application instance (lowest to highest precedence; later sources override duplicate names; paths are JSON-quoted):\n{roots}\n\n{available}\n"
    )
}

/// What an agent receives beyond the application's own tools.
pub(crate) struct Extensions {
    pub skills: SkillRegistry,
    pub skill_roots: Vec<std::path::PathBuf>,
    pub mcp: Vec<(String, roomkit_mcp::McpServer)>,
    /// Whether every tool call must carry an intent.
    pub intent_required: bool,
}

pub(crate) async fn create(
    root: &std::path::Path,
    resources: &Resources,
    profile: &Agent,
    provider: OpenAiConfig,
    extensions: Extensions,
) -> Result<(OpenAiAgent, Inventory)> {
    profile.validate()?;
    let Extensions {
        skills,
        skill_roots,
        mcp,
        intent_required,
    } = extensions;
    let mutations = resources.mutations.clone();
    let mut shell_limits = ShellLimits::default();
    shell_limits.max_output_bytes = 8192;
    assemble!(ChannelId::new(&profile.id)?, profile, provider, skills, &skill_roots, mcp, resources.instructions, resources.files, intent_required;
        outline = CodeOutlineToolset::new(CodeOutlineTool::new(root).await?),
        list = ListToolset::new(ListTool::new(root).await?),
        glob = GlobToolset::new(GlobTool::new(root).await?),
        grep = GrepToolset::new(GrepTool::new(root).await?),
        read = ReadToolset::new(ReadTool::new(root).await?),
        diff = DiffToolset::new(DiffTool::new(root).await?),
        write = WriteToolset::new(WriteTool::from_workspace(mutations.clone())),
        edit = EditToolset::new(EditTool::from_workspace(mutations.clone())),
        patch = PatchToolset::new(PatchTool::from_workspace(mutations)),
        shell = ShellToolset::new(ShellTool::new(root).await?.with_limits(shell_limits)?.with_output_store(resources.outputs.clone())),
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
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_instructions_count_as_the_engine_keeps_them() {
        assert_eq!(instruction_chars("calc", None), 0);
        assert_eq!(instruction_chars("calc", Some("  \n ")), 0);
        let short = instruction_chars("calc", Some("  Use add.  "));
        assert_eq!(short, instruction_chars("calc", Some("Use add.")));
        let long = instruction_chars("calc", Some(&"x".repeat(40_000)));
        assert_eq!(long - short, SERVER_INSTRUCTIONS_MAX - "Use add.".len());
        // Characters are cut, bytes are counted, like every other part.
        let accented = instruction_chars("calc", Some(&"é".repeat(40_000)));
        assert_eq!(accented - long, SERVER_INSTRUCTIONS_MAX);
    }

    #[test]
    fn tool_schemas_count_the_intent_property_and_the_function_wrapper() {
        let tool = ToolDefinition::new(
            "add",
            "Add two numbers",
            serde_json::json!({"type": "object", "properties": {"a": {"type": "number"}}}),
        );
        let bare = tool.name.len() + tool.description.len() + tool.parameters.to_string().len();
        let sent = schema_chars(std::slice::from_ref(&tool), false);
        assert!(sent > bare + "_roomkit_intent".len() + r#"{"type":"function"}"#.len());
    }
}
