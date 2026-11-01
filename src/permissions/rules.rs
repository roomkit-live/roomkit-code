//! What the application lets each tool do, in one table: whether it runs
//! without an approval in Ask and Yolo, whether Plan permits it, and how its
//! approval card presents it. A tool missing from the table never runs.

use roomkit::ToolCallEvent;

/// One tool's treatment, before command hooks add their own decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rule {
    /// Ask and Yolo: runs without an approval, unless a hook asks for one.
    pub direct: bool,
    /// Plan: permitted, with the same approval as in Ask.
    pub plan: bool,
    /// The first line of its approval card.
    pub label: &'static str,
}

const CHANGE: &str = "Proposed workspace change · exact tool arguments";

/// Reads of the workspace, the agent's processes and archives, its skills and
/// the guide, and the agent's own plans, which it also writes.
const READ: Rule = Rule {
    direct: true,
    plan: true,
    label: CHANGE,
};
/// Cleanup of the agent's own processes and archives: Plan permits neither.
const CLEANUP: Rule = Rule {
    direct: true,
    plan: false,
    label: CHANGE,
};
const MUTATION: Rule = Rule {
    direct: false,
    plan: false,
    label: CHANGE,
};
const COMMAND: Rule = Rule {
    direct: false,
    plan: false,
    label: "Command · runs with your user permissions (not sandboxed)",
};
/// Every MCP tool, whatever its name says: servers are configured outside
/// the application and their effects are unknown.
const MCP: Rule = Rule {
    direct: false,
    plan: false,
    label: "MCP server tool · may access external resources or modify data; exact arguments",
};

/// Every tool an agent can select, MCP tools and patch previews aside (see
/// `rule_for`).
const RULES: &[(&str, Rule)] = &[
    ("activate_skill", READ),
    ("read_skill_reference", READ),
    ("list", READ),
    ("glob", READ),
    ("grep", READ),
    ("read", READ),
    ("diff", READ),
    ("code_outline", READ),
    ("process_status", READ),
    ("process_read", READ),
    ("output_read", READ),
    ("plan_create", READ),
    ("plan_read", READ),
    ("plan_list", READ),
    ("plan_update", READ),
    ("plan_delete", READ),
    ("docs_list", READ),
    ("docs_search", READ),
    ("docs_read", READ),
    ("process_stop", CLEANUP),
    ("output_forget", CLEANUP),
    // Runs as a read while it only previews; see `rule_for`.
    ("patch", MUTATION),
    ("write", MUTATION),
    ("edit", MUTATION),
    ("shell", COMMAND),
    ("process_start", COMMAND),
    (
        "process_write",
        Rule {
            direct: false,
            plan: false,
            label: "Process input · may trigger further actions in the running command",
        },
    ),
    (
        "plan_mode_enter",
        Rule {
            direct: false,
            plan: false,
            label: "Mode change · this conversation moves to Plan: no project changes or commands until you approve a plan",
        },
    ),
    // Its card shows the stored plan (`modes::approve_exit`).
    (
        "plan_mode_exit",
        Rule {
            direct: false,
            plan: true,
            label: "Plan approval",
        },
    ),
    // Network reads keep Ask's approval, even in Plan.
    (
        "web_fetch",
        Rule {
            direct: false,
            plan: true,
            label: "Web request · retrieves this public URL",
        },
    ),
    (
        "web_search",
        Rule {
            direct: false,
            plan: true,
            label: "Web search · sends this query to Exa's anonymous service",
        },
    ),
];

/// The rules as the system prompt states them: the lists come from the table,
/// so the prompt follows it; the clauses on patch previews and MCP tools
/// state `rule_for`.
pub(crate) fn summary() -> String {
    let names = |keep: fn(&Rule) -> bool| {
        let names: Vec<&str> = RULES
            .iter()
            .filter(|(_, rule)| keep(rule))
            .map(|(name, _)| *name)
            .collect();
        match names.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            _ => names.join(""),
        }
    };
    format!(
        "In Ask mode these tools run without approval: {}, as does patch while it only previews. Every other tool, MCP tools included, asks for approval before it runs. In Yolo mode selected tools run without prompts. Plan permits only these tools without approval: {}, plus patch while it only previews; it also permits {}, with approval. A command hook may still refuse a call or require its approval.",
        names(|rule| rule.direct),
        names(|rule| rule.direct && rule.plan),
        names(|rule| !rule.direct && rule.plan),
    )
}

/// The rule for this call, or none: such a tool is blocked.
pub(crate) fn rule(event: &ToolCallEvent) -> Option<Rule> {
    rule_for(event.call.name.as_str(), &event.call.arguments)
}

fn rule_for(name: &str, arguments: &serde_json::Map<String, serde_json::Value>) -> Option<Rule> {
    // The tool's schema defines an omitted dry_run as true.
    if name == "patch" && arguments.get("dry_run").is_none_or(|value| value == true) {
        return Some(READ);
    }
    if name.starts_with("mcp__") {
        return Some(MCP);
    }
    RULES
        .iter()
        .find(|(listed, _)| *listed == name)
        .map(|(_, rule)| *rule)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A missing rule fails the test")]
    use super::*;
    use serde_json::{Map, json};

    fn arguments(value: serde_json::Value) -> Map<String, serde_json::Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn every_tool_an_agent_can_select_has_a_rule() {
        let others = [
            "activate_skill",
            "read_skill_reference",
            "plan_mode_enter",
            "plan_mode_exit",
        ];
        for name in crate::profiles::model::TOOLS.iter().chain(&others) {
            assert!(rule_for(name, &Map::new()).is_some(), "{name} has no rule");
        }
    }

    #[test]
    fn unknown_tools_have_no_rule_and_mcp_tools_always_ask() {
        assert_eq!(rule_for("rm_rf", &Map::new()), None);
        let mcp = rule_for("mcp__files__read", &Map::new()).unwrap();
        assert!(!mcp.direct && !mcp.plan);
    }

    #[test]
    fn a_patch_runs_directly_only_as_a_preview() {
        for preview in [json!({}), json!({"dry_run": true})] {
            assert_eq!(rule_for("patch", &arguments(preview)), Some(READ));
        }
        let applied = rule_for("patch", &arguments(json!({"dry_run": false}))).unwrap();
        assert!(!applied.direct && !applied.plan);
    }

    #[test]
    fn plan_permits_reads_and_web_requests_but_no_effect() {
        let plan: Vec<&str> = RULES
            .iter()
            .filter(|(_, rule)| rule.plan)
            .map(|(name, _)| *name)
            .collect();
        for effect in ["write", "edit", "shell", "process_start", "process_write"] {
            assert!(!plan.contains(&effect), "{effect}");
        }
        for cleanup in ["process_stop", "output_forget"] {
            let rule = rule_for(cleanup, &Map::new()).unwrap();
            assert!(rule.direct && !rule.plan, "{cleanup}");
        }
        for web in ["web_fetch", "web_search"] {
            let rule = rule_for(web, &Map::new()).unwrap();
            assert!(!rule.direct && rule.plan, "{web}");
        }
    }
}
