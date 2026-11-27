//! The tools an agent is assembled with, and their assembly; `preamble`
//! builds its system prompt and measures what its requests carry.

use crate::providers::CodeProvider;
use roomkit::model::{ChannelId, RetryPolicy};
use roomkit_ai::{
    AiChannel, AiChannelOptions, AiTool, ImageResolver, SkillRegistry, Tool, ToolEntry,
    ToolHandler, compose_tool_handlers,
};
use roomkit_tools_code::CodeOutlineTool;
use roomkit_tools_docs::{DocsBackend, DocsListTool, DocsReadTool, DocsSearchTool};
use roomkit_tools_fs::{
    DiffTool, EditTool, GlobTool, GrepTool, ListTool, PatchTool, ReadTool, Workspace, WriteTool,
};
use roomkit_tools_plans::{
    PlanCreateTool, PlanDeleteTool, PlanListTool, PlanReadTool, PlanUpdateTool,
};
use roomkit_tools_process::{
    OutputLimits, OutputStore, ProcessLimits, ProcessSession, ShellLimits, ShellTool,
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
mod resources;
mod tools;
pub(crate) use resources::*;
pub(crate) use tools::*;

use preamble::preamble;
pub(crate) use preamble::{Composition, Inventory};

/// The agent the application runs: an AI channel answering through an
/// configured native provider.
pub(crate) type CodeAgent = AiChannel<crate::project_memory::AgentProvider>;

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

/// What an agent receives beyond the application's own tools.
pub(crate) struct Extensions {
    pub project_memory: roomkit_tools_memory::MemoryTool,
    pub history: Option<crate::project_memory::History>,
    pub executor_inventory: Option<crate::remote::pool::Inventory>,
    pub skills: SkillRegistry,
    pub skill_roots: Vec<std::path::PathBuf>,
    pub mcp: Vec<crate::profiles::McpTools>,
    /// Whether the tool calls that ask for an intent must carry one.
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
    /// What serves `ask_user`, shared by every agent so that the engine
    /// announces its requests (`crate::ask_user`); none when nobody watches
    /// the launch, which then offers no such tool.
    pub human_input: Option<roomkit_ai::HumanInputToolHandler>,
    /// The conversation the agent is assembled for, where it runs with a
    /// model or an effort that is not its own: what it registers to compact
    /// its history is then that conversation's alone.
    pub conversation: Option<String>,
}

pub(crate) async fn create(
    root: &std::path::Path,
    channel: &str,
    resources: &Resources,
    profile: &Agent,
    provider: CodeProvider,
    extensions: Extensions,
) -> Result<(CodeAgent, Inventory)> {
    profile.validate()?;
    let unattended = extensions.unattended;
    // An unattended launch's agent is told nothing of its jobs' end.
    let limits = shell_limits(unattended);
    let shell = preamble::ShellFacts {
        maximum_ms: limits.max_timeout_ms,
        moves_on_timeout: limits.background_on_timeout,
        told_ends: !unattended,
    };
    let policy = extensions.policy.clone();
    let mut tools = native_tools(root, resources, policy).await?;
    let recall = tools::add_memory(&mut tools, profile, &extensions);
    let mut tools = selected(profile, tools, (unattended, extensions.planning));
    if !unattended && channel != crate::names::REMOTE_AGENT {
        tools.extend(
            resources
                .messages
                .tools(extensions.executor_inventory.is_some())
                .into_iter()
                .map(|tool| (tool.definition(), tool)),
        );
    }
    if let Some(inventory) = extensions.executor_inventory {
        add_pool_tools(&mut tools, inventory, channel, profile, root, resources);
    }
    let native: Vec<AiTool> = tools
        .iter()
        .map(|(definition, _)| definition.clone())
        .collect();
    let named = native.iter().map(|tool| tool.name.as_str());
    resources.turn_ends.agent_reaches(&profile.id, named);
    let Extensions {
        skills,
        skill_roots,
        mcp,
        intent_required,
        max_tool_rounds,
        delegation,
        images,
        ceiling,
        human_input,
        conversation,
        ..
    } = extensions;
    let preamble = preamble(
        profile,
        native,
        &skills,
        &skill_roots,
        &mcp,
        (
            intent_required,
            engine_tools(delegation, human_input.as_ref()),
        ),
        shell,
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
        tool_call_intents_only: Some(intent_tools(&mcp)),
        tool_timeout: Some(crate::timeouts::tool_call(unattended)),
        // A coding turn runs as long as its calls and approvals take, as
        // before: the engine's five-minute bound on a turn's tool loop would
        // end it before the calls of its next round run.
        tool_loop_timeout: None,
        max_tool_rounds,
        max_context_events: HISTORY,
        retry_policy: Some(retry_policy()),
        reasoning_effort: profile.reasoning_effort.clone(),
        thinking_budget: profile.thinking_budget.map(i64::from),
        memory: memory(
            (ceiling, &profile.id, channel),
            &provider,
            (resources, conversation.as_deref()),
        ),
        evict_threshold_tokens: stored_result_threshold(ceiling),
        image_resolver: Some(images),
        human_input_handler: human_input,
        // Native tools stay declared; only MCP tools wait behind find_tools.
        tool_search: Some(preamble.hides_mcp),
        tool_search_pinned: preamble.pinned,
        ..AiChannelOptions::default()
    };
    let provider = crate::project_memory::AgentProvider::new(provider, recall);
    let agent = AiChannel::new(ChannelId::new(channel)?, provider, options)?;
    Ok((agent, preamble.inventory))
}

/// The memory of `agent`, whose history compacts under `ceiling` tokens,
/// summarized by its own model, which also compacts its conversations
/// outside its turns; none keeps its window of `HISTORY` messages.
fn memory(
    (ceiling, profile, channel): (Option<u64>, &str, &str),
    provider: &CodeProvider,
    (resources, conversation): (&Resources, Option<&str>),
) -> Option<Arc<dyn roomkit_ai::memory::MemoryProvider>> {
    let ceiling = ceiling?;
    let summarizer = provider.clone();
    let compactions = resources.compactions.clone();
    let memory = Arc::new(crate::compaction::TieredMemory::new(
        summarizer,
        (ceiling, channel),
        compactions.clone(),
    ));
    // Profile IDs identify the summarizer; history is authored by the transport
    // channel, which all profiles of a remote worker share.
    match conversation {
        Some(room) => compactions.register_in(room, profile, memory.clone()),
        None => compactions.register(profile, memory.clone()),
    }
    Some(memory)
}

/// The most estimated tokens of a tool's result an agent reads whole,
/// whatever its window: the largest page a native tool returns, the 50 KiB
/// of `read` and `grep`, with what its envelope adds.
const STORED_RESULT_MAX_TOKENS: u64 = 16_000;

/// The estimated tokens past which the engine hands an agent a preview of a
/// tool's result and stores the whole for `read_stored_result`: a tenth of
/// the tokens its history compacts under, `ceiling`, so that one result never
/// takes more of the window, between the engine's own threshold, which an
/// agent without a known window keeps, and [`STORED_RESULT_MAX_TOKENS`]. At
/// the engine's 5,000 tokens, a file of 450 lines took four calls to read,
/// each page a JSON string.
fn stored_result_threshold(ceiling: Option<u64>) -> u64 {
    let engine = AiChannelOptions::default().evict_threshold_tokens;
    let most = STORED_RESULT_MAX_TOKENS.max(engine);
    ceiling.map_or(engine, |ceiling| (ceiling / 10).clamp(engine, most))
}

#[cfg(test)]
mod stored_result_tests {
    use super::{STORED_RESULT_MAX_TOKENS, stored_result_threshold};

    /// The engine estimates a quarter of the characters, plus one.
    const fn tokens(bytes: u64) -> u64 {
        bytes / 4 + 1
    }

    #[test]
    fn a_result_is_read_whole_within_a_tenth_of_the_window_and_a_native_page() {
        let engine = roomkit_ai::AiChannelOptions::default().evict_threshold_tokens;
        // Without a known window, and in a small one, the engine's threshold.
        assert_eq!(stored_result_threshold(None), engine);
        assert_eq!(stored_result_threshold(Some(32_768)), engine);
        assert_eq!(stored_result_threshold(Some(100_000)), 10_000);
        // A full page of `read` or `grep`, 50 KiB, passes whole from there on.
        let page = tokens(50 * 1024);
        assert!(stored_result_threshold(Some(131_072)) >= page);
        assert!(STORED_RESULT_MAX_TOKENS >= page);
        // No window makes a result past the largest native page pass whole.
        assert_eq!(
            stored_result_threshold(Some(1_000_000)),
            STORED_RESULT_MAX_TOKENS
        );
    }
}
