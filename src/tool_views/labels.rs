//! The activity line of a call that carries no intention: the call in a few
//! words, read from its arguments. The calls whose arguments say nothing of
//! their purpose ask the model for an intention instead
//! ([`crate::agent::asks_intent`]).

use roomkit_tui::protocol::ToolCall;
use serde_json::Value;

/// `call` in a few words; `None` leaves the activity line to the tool's name.
pub(super) fn activity(call: &ToolCall) -> Option<String> {
    let text = |name: &str| call.arguments.get(name).and_then(Value::as_str);
    let on = |verb: &str, name: &str| text(name).map(|value| format!("{verb} {value}"));
    match call.name.as_str() {
        "read" => on("Read", "path"),
        "code_outline" => on("Outline", "path"),
        "list" => Some(format!("List {}", text("path").unwrap_or("."))),
        "glob" => on("Find", "pattern"),
        "grep" => on("Search for", "pattern"),
        "write" => on("Write", "path"),
        "edit" => on("Edit", "path"),
        "diff" => Some(format!(
            "Compare {} and {}",
            text("old_path")?,
            text("new_path")?
        )),
        "patch" => {
            let changes = call.arguments.get("changes")?.as_array()?.len();
            let plural = if changes == 1 { "" } else { "s" };
            Some(format!("Patch {changes} file{plural}"))
        }
        "web_fetch" => on("Fetch", "url"),
        "web_search" => on("Search the web for", "query"),
        "process_status" => Some(text("process_id").map_or_else(
            || "List processes".to_owned(),
            |id| format!("Check process {id}"),
        )),
        "process_read" => on("Read process", "process_id"),
        "process_write" => on("Write to process", "process_id"),
        "process_stop" => on("Stop process", "process_id"),
        "output_read" => on("Read output", "archive_id"),
        "output_forget" => on("Forget output", "archive_id"),
        "plan_create" => on("Create plan", "title"),
        "plan_read" => on("Read plan", "plan_id"),
        "plan_list" => Some("List plans".to_owned()),
        "plan_update" => on("Update plan", "plan_id"),
        "plan_delete" => on("Delete plan", "plan_id"),
        "docs_list" => Some("List the guide".to_owned()),
        "docs_search" => on("Search the guide for", "query"),
        "docs_read" => on("Read the guide's", "document_id"),
        "activate_skill" => on("Load skill", "name"),
        "read_skill_reference" => on("Read skill reference", "filename"),
        _ => None,
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use roomkit_tui::protocol::{ToolCall, ToolState};
    use serde_json::{Value, json};

    use super::activity;

    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall::new("c1", name, arguments, ToolState::Running)
    }

    #[test]
    fn a_call_reads_from_its_arguments() {
        for (name, arguments, label) in [
            ("read", json!({"path": "src/main.rs"}), "Read src/main.rs"),
            ("list", json!({}), "List ."),
            ("grep", json!({"pattern": "todo"}), "Search for todo"),
            (
                "diff",
                json!({"old_path": "a", "new_path": "b"}),
                "Compare a and b",
            ),
            ("patch", json!({"changes": [1, 2]}), "Patch 2 files"),
            ("patch", json!({"changes": [1]}), "Patch 1 file"),
            ("process_status", json!({}), "List processes"),
            (
                "process_status",
                json!({"process_id": "p1"}),
                "Check process p1",
            ),
            ("plan_list", json!({}), "List plans"),
        ] {
            assert_eq!(activity(&call(name, arguments)).unwrap(), label);
        }
    }

    #[test]
    fn a_tool_or_an_argument_it_does_not_know_leaves_the_name() {
        assert_eq!(activity(&call("read", json!({}))), None);
        assert_eq!(activity(&call("shell", json!({"command": "ls"}))), None);
        assert_eq!(activity(&call("mcp__s__t", json!({}))), None);
    }
}
