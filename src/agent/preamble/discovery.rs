//! Code's selection of immediately useful tools and its task-oriented inventory.
//! RoomKit owns search, schema revelation, per-room usage and permission gates.
use super::{DISCOVERY_TOOLS, READ_STORED_RESULT, SEARCH_PREAMBLE, schema_chars};
use crate::names::{
    ANSWER_PEER, ASK_PEER, CARGO_TEST, LIST_EXECUTORS, LIST_SESSIONS, SEND_MESSAGE,
};
use roomkit_ai::AiTool;
use std::collections::BTreeMap;

pub(super) fn immediate(tool: &AiTool) -> bool {
    matches!(
        tool.name.as_str(),
        "read"
            | "list"
            | "glob"
            | "grep"
            | "code_outline"
            | "shell"
            | "edit"
            | "write"
            | "memory"
            | "docs_search"
            | SEND_MESSAGE
            | LIST_SESSIONS
            | LIST_EXECUTORS
            | ASK_PEER
            | ANSWER_PEER
            | CARGO_TEST
    )
}

pub(super) fn worth_deferring(
    native: &[AiTool],
    intent: bool,
    listed: &[AiTool],
    search_already_active: bool,
) -> bool {
    // A small profile should not spend more on explaining discovery than it saves.
    let savings = schema_chars(native.iter().filter(|tool| !immediate(tool)), intent);
    if search_already_active {
        return savings > 0;
    }
    savings > SEARCH_PREAMBLE + DISCOVERY_TOOLS + manifest(listed).len()
}

pub(super) fn manifest(tools: &[AiTool]) -> String {
    let mut groups = BTreeMap::<&str, Vec<&str>>::new();
    for tool in tools {
        groups.entry(usage(tool)).or_default().push(&tool.name);
    }
    groups
        .entry("Read a stored tool result")
        .or_default()
        .push(READ_STORED_RESULT);
    let mut lines = vec![
        "Current capability inventory for this agent. This is the complete set of tools available here. For a listed action whose schema is absent, call find_tools with its exact English tool name before substituting shell or asking the user. list_tools gives an overview.".to_owned(),
    ];
    for (usage, names) in groups {
        lines.push(format!("{usage} — {}", names.join(", ")));
    }
    let has = |name| tools.iter().any(|t| t.name == name);
    if has("docs_search") {
        lines.push("For questions about using RoomKit Code, start with docs_search to look up the installed guide.".into());
    }
    if has(LIST_SESSIONS) && has("delegate_task") && tools.iter().any(can_address) {
        lines.push("To follow up on work already delegated, send_message to its child_room_id. Use list_sessions if you need to find the conversation.".into());
    }
    lines.join("\n")
}

fn can_address(tool: &AiTool) -> bool {
    tool.name == SEND_MESSAGE && tool.parameters.pointer("/properties/to").is_some()
}

fn usage(tool: &AiTool) -> &'static str {
    match tool.name.as_str() {
        "list" | "glob" | "grep" | "read" | "code_outline" => "Explore source files",
        "edit" | "write" | "patch" | "diff" => "Change or compare files",
        "shell" => "Run a shell command",
        "memory" if tool.parameters.pointer("/properties/reference").is_some() => {
            "Recall earlier workspace conversations or use project notes"
        }
        "memory" => "Use project notes",
        crate::names::ASK_USER => "Ask the user a question",
        "delegate_task" => "Delegate new work",
        "task_status" | "cancel_task" => "Track delegated work",
        LIST_EXECUTORS => "Inspect executor machines and profiles",
        LIST_SESSIONS => "Find conversations you can contact",
        SEND_MESSAGE if can_address(tool) => "Contact a session or reply",
        SEND_MESSAGE => "Reply to an incoming session message",
        ASK_PEER | ANSWER_PEER => "Consult a sibling task",
        CARGO_TEST => "Test the assigned Git revision",
        name if name.starts_with("plan_mode_") => "Request a change of conversation mode",
        name if name.starts_with("plan_") => "Manage this conversation's plans",
        name if name.starts_with("process_") => "Run and monitor background processes",
        name if name.starts_with("output_") => "Manage archived command output",
        name if name.starts_with("docs_") => "Consult the RoomKit Code guide",
        name if name.starts_with("web_") => "Research public web sources",
        "activate_skill" | "read_skill_reference" | "run_skill_script" => "Use selected skills",
        name if name.starts_with("mcp__") => "Use selected MCP integrations",
        _ => "Other available tools",
    }
}
