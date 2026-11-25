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

/// Reads of the workspace, the agent's processes and archives, the outcomes the
/// engine stored, its skills, its tool catalogue and the guide, and the agent's
/// own plans, which it also writes.
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
/// The engine's delegation: handing a task to another agent, following and
/// cancelling the conversation's tasks. The worker takes the conversation's
/// mode and is refused what needs an approval (`Policy::adopt`), so handing
/// the task over asks none, in Plan too.
const DELEGATION: Rule = Rule {
    direct: true,
    plan: true,
    label: CHANGE,
};
/// A new message to another session: its agent then acts on it, in that
/// session's own mode, which may run what this one's Plan does not. Plan
/// does not start it; a reply, or a message to this conversation's own
/// task, stays delegation's.
const SESSION_MESSAGE: Rule = Rule {
    direct: true,
    plan: false,
    label: CHANGE,
};

/// Whether a `send_message` call with `arguments` writes a new message to
/// another session: neither a reply nor a message to one of the
/// conversation's tasks, which is addressed as `task:<id>` or by its
/// room, as `own_task` knows it. An empty string is an argument left out,
/// as some providers send it.
fn starts_a_session(
    arguments: &serde_json::Map<String, serde_json::Value>,
    own_task: &dyn Fn(&str) -> bool,
) -> bool {
    recipient(arguments).is_some_and(|to| !to.starts_with("task:") && !own_task(to))
}

/// Whom a `send_message` call with `arguments` writes a new message to;
/// none for a reply.
pub(crate) fn recipient(arguments: &serde_json::Map<String, serde_json::Value>) -> Option<&str> {
    let given = |key: &str| {
        arguments
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    given("to").filter(|_| given("reply_to").is_none())
}

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
    // Asking the person changes nothing: it needs no approval, in Plan too.
    (crate::names::ASK_USER, READ),
    ("activate_skill", READ),
    ("read_skill_reference", READ),
    ("find_tools", READ),
    ("list_tools", READ),
    (crate::names::LIST_EXECUTORS, READ),
    ("list", READ),
    ("glob", READ),
    ("grep", READ),
    ("read", READ),
    ("diff", READ),
    ("code_outline", READ),
    ("process_status", READ),
    ("process_read", READ),
    ("output_read", READ),
    // The engine's own, which every agent declares to page an outcome the
    // engine stored in its place (roomkit-rs RMK-1228).
    ("read_stored_result", READ),
    ("plan_create", READ),
    ("plan_read", READ),
    ("plan_list", READ),
    ("plan_update", READ),
    ("plan_delete", READ),
    ("docs_list", READ),
    ("docs_search", READ),
    ("docs_read", READ),
    (crate::names::ASK_PEER, DELEGATION),
    (crate::names::ANSWER_PEER, DELEGATION),
    (crate::names::SEND_MESSAGE, DELEGATION),
    (crate::names::LIST_SESSIONS, READ),
    ("delegate_task", DELEGATION),
    ("task_status", DELEGATION),
    ("cancel_task", DELEGATION),
    ("process_stop", CLEANUP),
    ("output_forget", CLEANUP),
    // Runs as a read while it only previews; see `rule_for`.
    ("patch", MUTATION),
    ("write", MUTATION),
    ("edit", MUTATION),
    ("shell", COMMAND),
    (crate::names::CARGO_TEST, COMMAND),
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

/// The rules as the system prompt states them, for the tools `has` names:
/// the lists come from the table, so the prompt follows it, and name no tool
/// the agent lacks; the clauses on patch previews and MCP tools state
/// `rule_for`.
pub(crate) fn summary(has: impl Fn(&str) -> bool) -> String {
    let names = |keep: fn(&Rule) -> bool| {
        let names: Vec<&str> = RULES
            .iter()
            .filter(|(name, rule)| keep(rule) && has(name))
            .map(|(name, _)| *name)
            .collect();
        match names.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            Some(_) => names.join(""),
            None => "none".into(),
        }
    };
    let messages = if has(crate::names::SEND_MESSAGE) {
        " In Plan, send_message only replies (reply_to) or writes to this conversation's own tasks (to: task:…); it starts no message to another session."
    } else {
        ""
    };
    format!(
        "In Ask mode these tools run without approval: {}, as does patch while it only previews. Every other tool, MCP tools included, asks for approval before it runs. In Yolo mode selected tools run without prompts. Plan permits only these tools without approval: {}, plus patch while it only previews; it also permits {}, with approval.{messages} A command hook may still refuse a call or require its approval.",
        names(|rule| rule.direct),
        names(|rule| rule.direct && rule.plan),
        names(|rule| !rule.direct && rule.plan),
    )
}

/// The rule for this call, or none: such a tool is blocked. `own_task`
/// tells whether a room is one of the calling conversation's own tasks.
pub(crate) fn rule(event: &ToolCallEvent, own_task: &dyn Fn(&str) -> bool) -> Option<Rule> {
    rule_for(event.name.as_str(), &event.arguments, own_task)
}

fn rule_for(
    name: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
    own_task: &dyn Fn(&str) -> bool,
) -> Option<Rule> {
    // The tool's schema defines an omitted dry_run as true.
    if name == "patch" && arguments.get("dry_run").is_none_or(|value| value == true) {
        return Some(READ);
    }
    if name.starts_with("mcp__") {
        return Some(MCP);
    }
    if name == crate::names::SEND_MESSAGE && starts_a_session(arguments, own_task) {
        return Some(SESSION_MESSAGE);
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

    #[test]
    fn the_summary_names_only_the_tools_an_agent_has() {
        let summary = summary(|name| name == "read" || name == "shell");
        assert!(
            summary.contains("without approval: read, as does patch"),
            "{summary}"
        );
        assert!(!summary.contains("plan_create"), "{summary}");
        assert!(!summary.contains("shell"), "{summary}");
        let none = summary_of_nothing();
        assert!(none.contains("without approval: none,"), "{none}");
    }

    fn summary_of_nothing() -> String {
        summary(|_| false)
    }

    fn arguments(value: serde_json::Value) -> Map<String, serde_json::Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn every_tool_an_agent_can_select_has_a_rule() {
        let others = [
            "activate_skill",
            "read_skill_reference",
            "find_tools",
            "list_tools",
            "read_stored_result",
            "plan_mode_enter",
            "plan_mode_exit",
        ];
        for name in crate::profiles::model::TOOLS.iter().chain(&others) {
            assert!(
                rule_for(name, &Map::new(), &|_| false).is_some(),
                "{name} has no rule"
            );
        }
    }

    #[test]
    fn unknown_tools_have_no_rule_and_mcp_tools_always_ask() {
        assert_eq!(rule_for("rm_rf", &Map::new(), &|_| false), None);
        let mcp = rule_for("mcp__files__read", &Map::new(), &|_| false).unwrap();
        assert!(!mcp.direct && !mcp.plan);
    }

    #[test]
    fn a_patch_runs_directly_only_as_a_preview() {
        for preview in [json!({}), json!({"dry_run": true})] {
            assert_eq!(
                rule_for("patch", &arguments(preview), &|_| false),
                Some(READ)
            );
        }
        let applied = rule_for("patch", &arguments(json!({"dry_run": false})), &|_| false).unwrap();
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
            let rule = rule_for(cleanup, &Map::new(), &|_| false).unwrap();
            assert!(rule.direct && !rule.plan, "{cleanup}");
        }
        for web in ["web_fetch", "web_search"] {
            let rule = rule_for(web, &Map::new(), &|_| false).unwrap();
            assert!(!rule.direct && rule.plan, "{web}");
        }
    }

    /// Review finding: a Plan conversation's agent could have another
    /// session, in a mode of its own, do what Plan refuses.
    #[test]
    fn plan_starts_no_message_to_another_session() {
        let rule = |arguments: serde_json::Value| {
            let arguments = arguments.as_object().unwrap();
            rule_for(crate::names::SEND_MESSAGE, arguments, &|room| {
                room == "main::task-1"
            })
            .unwrap()
        };
        let new = rule(json!({"to": "review", "text": "Run the migration"}));
        assert!(new.direct && !new.plan);
        let empty_reply = rule(json!({"to": "review", "reply_to": "", "text": "x"}));
        assert!(!empty_reply.plan);
        for permitted in [
            json!({"reply_to": "session-msg-1", "text": "Done"}),
            json!({"to": "task:3", "text": "Also check the tests"}),
            // A task is also addressed by its room, which the policy knows.
            json!({"to": "main::task-1", "text": "Also check the tests"}),
        ] {
            let rule = rule(permitted);
            assert!(rule.direct && rule.plan);
        }
    }
}
