//! What an agent's requests carry before their messages: the system prompt
//! and the tool inventory, and how many characters each part takes. The
//! sidebar's context breakdown shows these measures.

use roomkit_ai::{AiChannelOptions, AiTool, SkillRegistry, TOOL_SEARCH_INFRA_TOOL_NAMES};
use std::path::PathBuf;

use crate::profiles::{McpTools, model::Agent};

const INSTRUCTIONS: &str = include_str!("coding-agent.txt");
const ACTIVITY_INSTRUCTIONS: &str = r#"For every tool call, include a short description of your next action in _roomkit_intent, in the user's language. Put action announcements there, not in a separate message. Keep messages for findings, questions, explanations and the final answer.
Use this only for the field described as "Shown to the user; never passed to the tool"; respect any tool-owned parameter of the same name. Never add an absent field."#;

/// What every request of an agent carries before its messages, in characters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Composition {
    /// The application's instructions, the agent's own and the tool inventory.
    pub system: usize,
    /// Native tool schemas, and the discovery tools' when MCP tools wait behind
    /// them.
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
const SKILLS_PREAMBLE: usize = 309 + 4;
const SERVERS_BLOCK: usize = 345 + 6;
/// What the engine adds when a catalogue waits behind its discovery tools, and
/// does not make public either: its preamble, and the two tools as sent.
const SEARCH_PREAMBLE: usize = 1388 + 2;
const DISCOVERY_TOOLS: usize = 1477;
/// The engine's own `read_stored_result`, which every agent declares to page
/// through an outcome the engine stored (roomkit-rs RMK-1228), and which it does
/// not make public either: its definition as sent, the intent optional, then
/// required.
const REREAD_TOOL: [usize; 2] = [1271, 1289];
/// The engine keeps at most this many characters of a server's instructions.
const SERVER_INSTRUCTIONS_MAX: usize = 8192;

/// Tools as an OpenAI-compatible request sends them: with the reserved intent
/// property the engine adds, required when the application requires it, each
/// wrapped as a function.
fn schema_chars(tools: &[AiTool], intent_required: bool) -> usize {
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
    /// Whether its MCP tools wait behind the discovery tools.
    pub hides_mcp: bool,
    /// The tools that never wait behind them: the native ones.
    pub pinned: Vec<String>,
}

/// The preamble of `profile` with its selected `native` tools, its skills
/// and its MCP servers.
pub(super) fn preamble(
    profile: &Agent,
    native: Vec<AiTool>,
    skills: &SkillRegistry,
    skill_roots: &[PathBuf],
    mcp: &[McpTools],
    intent_required: bool,
) -> Preamble {
    let hidden = hides_mcp(mcp, intent_required, profile.context_window);
    let mut tools = native.clone();
    tools.extend(skills.tool_definitions());
    for server in mcp {
        tools.extend(server.tools.iter().cloned());
    }
    // The engine serves every agent the delegation tools (RMK-1532): listed
    // and declared outright with the native ones, though no agent selects them.
    let delegation = crate::delegation::definitions();
    let served = [native.as_slice(), &delegation].concat();
    let listed = [tools.as_slice(), &delegation].concat();
    let base = base_prompt(profile, &listed, hidden);
    let context = skill_context(skills, skill_roots);
    let composition = measure(
        (&base, &context),
        &served,
        skills,
        mcp,
        intent_required,
        hidden,
    );
    Preamble {
        prompt: format!("{base}\n{context}\n{ACTIVITY_INSTRUCTIONS}"),
        inventory: Inventory { tools, composition },
        hides_mcp: hidden,
        pinned: served.into_iter().map(|tool| tool.name).collect(),
    }
}

/// The application's instructions, the agent's identity and instructions,
/// and the names of every tool it has, those it finds its other tools with
/// when they are `hidden`.
fn base_prompt(profile: &Agent, tools: &[AiTool], hidden: bool) -> String {
    let discovery = TOOL_SEARCH_INFRA_TOOL_NAMES.into_iter().filter(|_| hidden);
    let inventory = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .chain(discovery)
        .collect::<Vec<_>>()
        .join(", ");
    let instructions = INSTRUCTIONS.replacen("{permissions}", &crate::permissions::summary(), 1);
    format!(
        "{instructions}\nCurrent agent: {} (ID: {}). Answer as this selected participant. Other participants may have different tools and providers.\nAgent instructions:\n{}\nCurrent capability inventory (provided by the application for this run):\n{inventory}\nOnly tools in this inventory are available to you. Apply tool-specific guidance only when that tool is present.\n",
        profile.label, profile.id, profile.instructions
    )
}

/// Characters per part, as a request sends them. The engine's own texts are
/// counted with the part they introduce.
fn measure(
    (base, context): (&str, &str),
    native: &[AiTool],
    skills: &SkillRegistry,
    mcp: &[McpTools],
    intent_required: bool,
    hidden: bool,
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
            + only_if(hidden, SEARCH_PREAMBLE),
        tools: schema_chars(native, intent_required)
            + REREAD_TOOL[usize::from(intent_required)]
            + only_if(hidden, DISCOVERY_TOOLS),
        skills: context.len()
            + preamble
            + skills.to_prompt_xml().len()
            + schema_chars(&skills.tool_definitions(), intent_required),
        mcp: mcp
            .iter()
            .map(|server| {
                let size = instruction_chars(&server.label, server.instructions.as_deref())
                    + only_if(!hidden, schema_chars(&server.tools, intent_required));
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
            .filter_map(|name| skills.get_skill(name))
            .map(|skill| {
                format!(
                    "- {}: {}",
                    skill.name(),
                    serde_json::json!(skill.path.to_string_lossy())
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
mod tests {
    #![allow(clippy::unwrap_used, reason = "An invalid fixture fails the test")]
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
    fn the_prompt_states_the_shell_maximum_the_tool_enforces() {
        let maximum = crate::timeouts::SHELL_MAX_SECONDS * 1000;
        assert!(INSTRUCTIONS.contains(&format!("(maximum {maximum})")));
    }

    #[test]
    fn the_prompt_states_the_process_maximum_the_tool_enforces() {
        // A larger timeout_ms is refused, not capped.
        let maximum = crate::agent::process_limits().max_timeout_ms;
        assert!(INSTRUCTIONS.contains(&format!("up to one hour (maximum {maximum})")));
    }

    #[test]
    fn the_prompt_states_the_permission_rules_the_gate_applies() {
        assert_eq!(INSTRUCTIONS.matches("{permissions}").count(), 1);
        let profile: Agent = serde_json::from_value(serde_json::json!({
            "id": "code", "label": "Code", "provider": "local", "model": "m",
            "instructions": "", "tools": [],
        }))
        .unwrap();
        let prompt = base_prompt(&profile, &[], false);
        assert!(prompt.contains(&crate::permissions::summary()));
        assert!(!prompt.contains("{permissions}"));
    }

    #[test]
    fn mcp_tools_wait_past_the_engines_thresholds_whatever_the_native_tools_cost() {
        // The token cap, whatever the window.
        assert!(past_search_thresholds(8_001, 1, Some(10_000_000)));
        assert!(past_search_thresholds(8_001, 1, None));
        // Under the cap, a share of a known window; the count no longer matters.
        assert!(past_search_thresholds(7_000, 1, Some(65_500)));
        assert!(!past_search_thresholds(6_000, 40, Some(65_500)));
        // Under the cap with no window, the count: 29 native tools never add to it.
        assert!(!past_search_thresholds(100, 20, None));
        assert!(past_search_thresholds(100, 21, None));
        assert!(!past_search_thresholds(0, 0, None));
    }

    #[test]
    fn the_inventory_names_the_discovery_tools_only_when_they_are_declared() {
        let profile: Agent = serde_json::from_value(serde_json::json!({
            "id": "code", "label": "Code", "provider": "local", "model": "m",
            "instructions": "", "tools": [],
        }))
        .unwrap();
        let run = "for this run):\n";
        assert!(base_prompt(&profile, &[], false).contains(&format!("{run}\nOnly tools")));
        assert!(
            base_prompt(&profile, &[], true)
                .contains(&format!("{run}find_tools, list_tools\nOnly tools"))
        );
    }

    #[test]
    fn tool_schemas_count_the_intent_property_and_the_function_wrapper() {
        let tool = AiTool {
            name: "add".into(),
            description: "Add two numbers".into(),
            parameters: serde_json::json!({"type": "object", "properties": {"a": {"type": "number"}}}),
            tags: Vec::new(),
        };
        let bare = tool.name.len() + tool.description.len() + tool.parameters.to_string().len();
        let sent = schema_chars(std::slice::from_ref(&tool), false);
        assert!(sent > bare + "_roomkit_intent".len() + r#"{"type":"function"}"#.len());
    }
}
