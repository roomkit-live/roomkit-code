//! What an agent's requests carry before their messages: the system prompt
//! and the tool inventory, and how many characters each part takes. The
//! sidebar's context breakdown shows these measures.

use roomkit_ai::{AiTool, SkillRegistry};
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
    pub tools: Vec<AiTool>,
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

/// An agent's system prompt with its inventory and measures.
pub(super) struct Preamble {
    pub prompt: String,
    pub inventory: Inventory,
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
    let mut tools = native.clone();
    tools.extend(skills.tool_definitions());
    for server in mcp {
        tools.extend(server.tools.iter().cloned());
    }
    let base = base_prompt(profile, &tools);
    let context = skill_context(skills, skill_roots);
    let composition = measure(&base, &context, &native, skills, mcp, intent_required);
    Preamble {
        prompt: format!("{base}\n{context}\n{ACTIVITY_INSTRUCTIONS}"),
        inventory: Inventory { tools, composition },
    }
}

/// The application's instructions, the agent's identity and instructions,
/// and the names of every tool it has.
fn base_prompt(profile: &Agent, tools: &[AiTool]) -> String {
    let inventory = tools
        .iter()
        .map(|tool| tool.name.as_str())
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
    base: &str,
    context: &str,
    native: &[AiTool],
    skills: &SkillRegistry,
    mcp: &[McpTools],
    intent_required: bool,
) -> Composition {
    let servers = mcp
        .iter()
        .any(|server| instruction_chars(&server.label, server.instructions.as_deref()) > 0);
    let preamble = if skills.has_entries() {
        SKILLS_PREAMBLE
    } else {
        0
    };
    Composition {
        system: base.len() + ACTIVITY_INSTRUCTIONS.len() + if servers { SERVERS_BLOCK } else { 0 },
        tools: schema_chars(native, intent_required),
        skills: context.len()
            + preamble
            + skills.to_prompt_xml().len()
            + schema_chars(&skills.tool_definitions(), intent_required),
        mcp: mcp
            .iter()
            .map(|server| {
                let size = instruction_chars(&server.label, server.instructions.as_deref())
                    + schema_chars(&server.tools, intent_required);
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
        let maximum = roomkit_tools_process::ProcessLimits::default().max_timeout_ms;
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
        let prompt = base_prompt(&profile, &[]);
        assert!(prompt.contains(&crate::permissions::summary()));
        assert!(!prompt.contains("{permissions}"));
    }

    #[test]
    fn tool_schemas_count_the_intent_property_and_the_function_wrapper() {
        let tool = AiTool {
            name: "add".into(),
            description: "Add two numbers".into(),
            parameters: serde_json::json!({"type": "object", "properties": {"a": {"type": "number"}}}),
        };
        let bare = tool.name.len() + tool.description.len() + tool.parameters.to_string().len();
        let sent = schema_chars(std::slice::from_ref(&tool), false);
        assert!(sent > bare + "_roomkit_intent".len() + r#"{"type":"function"}"#.len());
    }
}
