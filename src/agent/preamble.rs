//! What an agent's requests carry before their messages: the system prompt
//! and the tool inventory, and how many characters each part takes. The
//! sidebar's context breakdown shows these measures.

use roomkit_ai::{AiChannelOptions, AiTool, SkillRegistry, TOOL_SEARCH_INFRA_TOOL_NAMES};
use std::path::PathBuf;

mod discovery;

use crate::profiles::{McpTools, model::Agent};

const INSTRUCTIONS: &str = include_str!("coding-agent.txt");
const ACTIVITY_INSTRUCTIONS: &str = r#"When a tool offers _roomkit_intent, write in it a short description of your next action, in the user's language. Put action announcements there, not in a separate message. Keep messages for findings, questions, explanations and the final answer.
Use this only for the field described as "Shown to the user; never passed to the tool"; respect any tool-owned parameter of the same name. Never add an absent field."#;

/// What every request of an agent carries before its messages, in characters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Composition {
    /// The application's instructions, the agent's own and, with hidden tools,
    /// the names of the tools.
    pub system: usize,
    /// Initially declared native schemas and any discovery tools.
    pub tools: usize,
    /// The skills manifest, its discovery context and the skill tools.
    pub skills: usize,
    /// Per MCP server, by label: its instructions, and its tool schemas unless
    /// they wait behind the discovery tools.
    pub mcp: Vec<(String, usize)>,
}

/// The tools an agent exposes, and what every one of its requests carries.
#[derive(Clone, Debug, Default)]
pub(crate) struct Inventory {
    pub tools: Vec<AiTool>,
    pub composition: Composition,
}

/// Bytes the engine adds to the system prompt, with their separating blank
/// lines: roomkit-ai's `skills::texts::PREAMBLE` and the heading and
/// introduction of its `server_instructions` block, which are not public.
/// `application::tests::breakdown` compares them with a request.
const SKILLS_PREAMBLE: usize = 244 + 4;
const SERVERS_BLOCK: usize = 345 + 6;
/// What the engine adds when a catalogue waits behind its discovery tools, and
/// does not make public either: its preamble, and the two tools as sent.
const SEARCH_PREAMBLE: usize = 1388 + 2;
const DISCOVERY_TOOLS: usize = 1044;
/// The engine's own `read_stored_result`, which every agent declares to page
/// through an outcome the engine stored (roomkit-rs RMK-1228), and which it does
/// not make public either: its definition as sent, which asks for no intent.
const REREAD_TOOL: usize = 1079;
/// That tool's name.
const READ_STORED_RESULT: &str = "read_stored_result";
/// The engine keeps at most this many characters of a server's instructions.
const SERVER_INSTRUCTIONS_MAX: usize = 8192;

/// Tools as an OpenAI-compatible request sends them: with the reserved intent
/// property the engine adds for the tools that ask for one, required when the
/// application requires it, each wrapped as a function.
fn schema_chars<'a>(tools: impl IntoIterator<Item = &'a AiTool>, intent_required: bool) -> usize {
    tools
        .into_iter()
        .map(|tool| {
            let shown = super::asks_intent(&tool.name)
                .then(|| {
                    roomkit::tool_intent::presented_parameters_with_requirement(
                        &tool.parameters,
                        intent_required,
                    )
                })
                .flatten();
            let parameters = shown.unwrap_or_else(|| tool.parameters.clone());
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

/// Whether the agent's MCP tools wait behind the engine's discovery tools.
/// The application decides, by the engine's own thresholds on what these tools
/// alone cost: behind a `base_url` the engine has no window to read and would
/// count the native tools too, searching a catalogue with nothing hidden.
fn hides_mcp(mcp: &[McpTools], intent_required: bool, window: Option<u64>) -> bool {
    let chars: usize = mcp
        .iter()
        .map(|server| schema_chars(&server.tools, intent_required))
        .sum();
    let count = mcp.iter().map(|server| server.tools.len()).sum();
    past_search_thresholds(chars.div_ceil(4), count, window)
}

/// The engine's thresholds for `tokens` of schemas of `count` tools: its token
/// cap, then its share of a known window, else its tool count.
fn past_search_thresholds(tokens: usize, count: usize, window: Option<u64>) -> bool {
    let engine = AiChannelOptions::default();
    let tokens = u64::try_from(tokens).unwrap_or(u64::MAX);
    if engine
        .tool_search_threshold_tokens
        .is_some_and(|cap| tokens > cap)
    {
        return true;
    }
    match window {
        Some(window) => tokens as f64 > window as f64 * engine.tool_search_threshold_pct / 100.0,
        None => count > engine.tool_search_threshold,
    }
}

/// An agent's system prompt with its inventory and measures, and what its
/// requests declare outright.
pub(super) struct Preamble {
    pub prompt: String,
    pub inventory: Inventory,
    /// Whether any catalogue is served through RoomKit Tool Search.
    pub search: bool,
    /// Selected tools declared from the first request.
    pub pinned: Vec<String>,
}

/// What the prompt states of an agent's shell and background jobs.
#[derive(Clone, Copy, Debug)]
pub(super) struct ShellFacts {
    /// The longest command the shell runs, in milliseconds.
    pub maximum_ms: u64,
    /// Whether the agent is told when its background jobs end.
    pub told_ends: bool,
    /// Whether a command still running at its timeout moves to the
    /// background as a process instead of being stopped.
    pub moves_on_timeout: bool,
}

/// What the agent reads of a command that reaches its timeout and is stopped.
const STOPPED_AT_TIMEOUT: &str =
    "A command is stopped after 10 seconds unless timeout_ms asks for more.";

/// What the agent reads of a command that reaches its timeout and moves to
/// the background.
const MOVED_AT_TIMEOUT: &str = "A command still running after 10 seconds, or the timeout_ms it asks for, is not stopped: it moves to the background as a process, and its result gives the process_id.";

/// What the agent reads of a shell command's background jobs, when it is
/// told of their end.
const SHELL_JOB_ENDS: &str = " A job a command leaves running in the background (&) runs on; once every such job has ended, the application tells you in a message.";

/// What the agent reads of its processes, when it is told of their end.
const PROCESS_ENDS: &str = " When a process ends, the application tells you in a message: you need not poll it to learn that.";

pub(super) struct ToolOptions {
    pub intent_required: bool,
    pub compact: bool,
    pub engine: Vec<AiTool>,
}

/// The preamble of `profile` with its selected `native` tools, its skills
/// and its MCP servers, whether every call carries an intent, the tools the
/// engine serves it (delegation, `ask_user`), and what it is told of its
/// shell.
pub(super) fn preamble(
    profile: &Agent,
    native: Vec<AiTool>,
    skills: &SkillRegistry,
    skill_roots: &[PathBuf],
    mcp: &[McpTools],
    options: ToolOptions,
    shell: ShellFacts,
) -> Preamble {
    let ToolOptions {
        intent_required,
        compact,
        engine,
    } = options;
    let deferred_mcp = hides_mcp(mcp, intent_required, profile.context_window);
    let mut tools = native.clone();
    tools.extend(skills.tool_definitions());
    for server in mcp {
        tools.extend(server.tools.iter().cloned());
    }
    // The engine serves the delegation tools (RMK-1532) and `ask_user`:
    // listed and declared outright with the native ones, though no agent
    // selects them.
    let listed = [tools.as_slice(), &engine].concat();
    let deferred_native =
        compact && discovery::worth_deferring(&native, intent_required, &listed, deferred_mcp);
    let search = deferred_native || deferred_mcp;
    let mut served = native;
    if deferred_native {
        served.retain(discovery::immediate);
    }
    served.extend(engine);
    let listing = match (search, compact) {
        (false, _) => ToolListing::Declared,
        (true, false) => ToolListing::Names,
        (true, true) => ToolListing::ByTask,
    };
    let base = base_prompt(profile, &listed, listing, shell);
    let context = skill_context(skills, skill_roots);
    let composition = measure(
        (&base, &context),
        &served,
        skills,
        mcp,
        intent_required,
        (search, deferred_mcp),
    );
    let mut pinned: Vec<_> = served.into_iter().map(|tool| tool.name).collect();
    if search && !deferred_mcp {
        pinned.extend(
            mcp.iter()
                .flat_map(|s| s.tools.iter().map(|t| t.name.clone())),
        );
    }
    Preamble {
        prompt: format!("{base}\n{context}\n{ACTIVITY_INSTRUCTIONS}"),
        inventory: Inventory { tools, composition },
        search,
        pinned,
    }
}

#[derive(Clone, Copy)]
enum ToolListing {
    Declared,
    Names,
    ByTask,
}

/// The application's instructions for an agent with `tools`, stating what it
/// is told of its shell, the agent's identity and instructions, and what it
/// is told of its tools beside their definitions.
fn base_prompt(
    profile: &Agent,
    tools: &[AiTool],
    listing: ToolListing,
    shell: ShellFacts,
) -> String {
    let available = available(tools, listing);
    let instructions = instructions(tools, !matches!(listing, ToolListing::Declared), shell);
    format!(
        "{instructions}\nCurrent agent: {} (ID: {}). Answer as this selected participant. Other participants may have different tools and providers.\nAgent instructions:\n{}\n{available}\n",
        profile.label, profile.id, profile.instructions
    )
}

/// What the agent is told of its tools beside their definitions. Their names
/// are listed only when some of them wait behind the discovery tools: the
/// definitions say the rest.
fn available(tools: &[AiTool], listing: ToolListing) -> String {
    const GUIDANCE: &str = "Apply tool-specific guidance only when that tool is present.";
    match listing {
        ToolListing::Declared => {
            format!("Only the tools you are given are available to you. {GUIDANCE}")
        }
        ToolListing::ByTask => discovery::manifest(tools),
        ToolListing::Names => {
            let names = tools
                .iter()
                .map(|tool| tool.name.as_str())
                .chain(TOOL_SEARCH_INFRA_TOOL_NAMES)
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Current capability inventory (provided by the application for this run):\n{names}\nOnly tools in this inventory are available to you. {GUIDANCE}"
            )
        }
    }
}

/// The application's instructions for an agent with `tools`, and the
/// discovery tools when its other tools are `hidden`. A paragraph about tools
/// an agent may lack opens with `{with name ...}`: it is kept, without that
/// tag, only when the agent has one of the tools it names (`prefix*` names any
/// tool starting so), so that a request carries no guidance for a tool it does
/// not declare (HITL cf0fc0eb).
fn instructions(tools: &[AiTool], hidden: bool, shell: ShellFacts) -> String {
    let has = |name: &str| tools.iter().any(|tool| tool.name == name);
    let names_a_tool = |tag: &str| match tag.strip_suffix('*') {
        Some(prefix) => tools.iter().any(|tool| tool.name.starts_with(prefix)),
        None => has(tag),
    };
    // The permission summary also names the tools the engine serves beside
    // the agent's own: every agent's read_stored_result, and the discovery
    // tools of hidden ones.
    let served = |name: &str| {
        has(name)
            || name == READ_STORED_RESULT
            || (hidden && TOOL_SEARCH_INFRA_TOOL_NAMES.contains(&name))
    };
    INSTRUCTIONS
        .split("\n\n")
        .filter_map(|paragraph| match tagged(paragraph) {
            Some((names, text)) => names.split(' ').any(names_a_tool).then_some(text),
            None => Some(paragraph),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
        .replacen("{permissions}", &crate::permissions::summary(served), 1)
        .replacen(
            "{shell_timeout}",
            if shell.moves_on_timeout {
                MOVED_AT_TIMEOUT
            } else {
                STOPPED_AT_TIMEOUT
            },
            1,
        )
        .replacen("{shell_maximum}", &shell.maximum_ms.to_string(), 1)
        .replacen(
            "{shell_job_ends}",
            if shell.told_ends { SHELL_JOB_ENDS } else { "" },
            1,
        )
        .replacen(
            "{process_ends}",
            if shell.told_ends { PROCESS_ENDS } else { "" },
            1,
        )
}

/// The tool names a paragraph's `{with name ...}` tag lists, and the
/// paragraph without it; none for a paragraph every agent reads.
fn tagged(paragraph: &str) -> Option<(&str, &str)> {
    paragraph.strip_prefix("{with ")?.split_once("} ")
}

/// Characters per part, as a request sends them. The engine's own texts are
/// counted with the part they introduce.
fn measure(
    (base, context): (&str, &str),
    native: &[AiTool],
    skills: &SkillRegistry,
    mcp: &[McpTools],
    intent_required: bool,
    (search, deferred_mcp): (bool, bool),
) -> Composition {
    let servers = mcp
        .iter()
        .any(|server| instruction_chars(&server.label, server.instructions.as_deref()) > 0);
    let preamble = if skills.has_entries() {
        SKILLS_PREAMBLE
    } else {
        0
    };
    let only_if = |condition: bool, chars: usize| if condition { chars } else { 0 };
    Composition {
        system: base.len()
            + ACTIVITY_INSTRUCTIONS.len()
            + only_if(servers, SERVERS_BLOCK)
            + only_if(search, SEARCH_PREAMBLE),
        tools: schema_chars(native, intent_required)
            + REREAD_TOOL
            + only_if(search, DISCOVERY_TOOLS),
        skills: context.len()
            + preamble
            + skills.to_prompt_xml().len()
            + schema_chars(&skills.tool_definitions(), intent_required),
        mcp: mcp
            .iter()
            .map(|server| {
                let size = instruction_chars(&server.label, server.instructions.as_deref())
                    + only_if(!deferred_mcp, schema_chars(&server.tools, intent_required));
                (server.label.clone(), size)
            })
            .collect(),
    }
}

/// Installation facts remain available before the first skill is enabled.
/// Only the selected packages' names and paths accompany the engine's metadata;
/// their instructions still load through activate_skill.
fn skill_context(skills: &SkillRegistry, roots: &[PathBuf]) -> String {
    let roots = roots
        .iter()
        .map(|path| format!("- {}", serde_json::json!(path.to_string_lossy())))
        .collect::<Vec<_>>()
        .join("\n");
    let available = if !skills.has_entries() {
        "No skills are currently available to this agent. Creation with available file tools and user configuration through /skills are still supported.".into()
    } else {
        let locations = skills
            .skill_names()
            .into_iter()
            // A local skill's lookup never fails; only a served one's can.
            .filter_map(|name| skills.get_skill(&name).ok().flatten())
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

#[cfg(test)]
mod tests;
