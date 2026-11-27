//! The commands the terminal lists for this application.
use super::*;

#[expect(
    clippy::too_many_lines,
    reason = "Debt recorded when the limit was set: split it, never grow it"
)]
pub(crate) fn commands() -> Vec<CommandSpec> {
    [
        (
            "update",
            "Check releases, install an update or restore the previous version",
            "[rollback]",
        ),
        (
            "instructions",
            "Inspect applicable AGENTS.md files and their scope",
            "[project-relative path]",
        ),
        (
            "hooks",
            "Inspect, enable and diagnose configured command hooks",
            "",
        ),
        (
            "plans",
            "Follow this agent's plans and inspect their steps",
            "[plan-id]",
        ),
        (
            "mcp",
            "Configure MCP servers and choose tools for this agent",
            "[list|add|connect <id>|disconnect <id>|enable <id>|disable <id>]",
        ),
        (
            "skills",
            "Inspect and enable skills for this agent, or request their use",
            "[list|reload|enable <name>|disable <name>|use <name> <task>]",
        ),
        (
            "shell",
            "Run a shell command without the model (also !command)",
            "<command>",
        ),
        (
            "downloads",
            "Manage GGUF downloads and installed models",
            "",
        ),
        (
            "model",
            "Choose the current agent's model among its connection's",
            "[model-id]",
        ),
        (
            "effort",
            "Choose reasoning effort for the current agent",
            "[default|low|medium|high|xhigh|max]",
        ),
        (
            "diagnostics",
            "Inspect recent engine errors and local audit logs",
            "",
        ),
        (
            "processes",
            "Inspect background processes and stop them (also F6)",
            "[process-id]",
        ),
        (
            "executors",
            "Inspect workers and configure this workspace’s executor pool (beta)",
            "[configure]",
        ),
        (
            "tasks",
            "See the tasks agents handed to other agents, and stop them",
            "[task-id]",
        ),
        ("back", "Return from a task's room to its conversation", ""),
        (
            "files",
            "List the files this conversation's agents changed or read",
            "",
        ),
        (
            "providers",
            "List, configure or delete shared provider connections",
            "[list|delete <id>]",
        ),
        (
            "agents",
            "List, select, configure or delete project agents",
            "[list|delete <id>]",
        ),
        (
            "tools",
            "Inspect tools actually registered by the application",
            "[tool]",
        ),
        (
            "mode",
            "Change permissions (also Shift+Tab)",
            "[ask|plan|yolo]",
        ),
        (
            "compact",
            "Summarize the conversation's earlier messages now, or once the turn ends",
            "",
        ),
        (
            "message",
            "Message another open project session",
            "[session] [text]",
        ),
        ("new", "Start a new conversation in this project", ""),
        ("rename", "Name this conversation", "<name>"),
        (
            "resume",
            "Choose a previous project session",
            "[session-id]",
        ),
    ]
    .into_iter()
    .map(|(name, description, arguments)| {
        let spec = CommandSpec::new(name, description).with_arguments(arguments);
        if name == "mode" {
            spec.with_choices(["ask", "plan", "yolo"].map(String::from).to_vec())
        } else {
            spec
        }
    })
    .collect()
}
