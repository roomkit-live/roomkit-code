//! What the model reads of a tool's result: its text.
//!
//! The tools answer with `{"data": ..., "text": ...}`. The model reads that
//! result again on every later request, so it gets the text alone; the data
//! goes to the views as the call's structured copy, which it never reads.

use roomkit::model::{HookResult, RoomContext};
use roomkit::{HookOptions, HookTrigger, RoomKit, ToolCallEvent};
use roomkit_sqlite::SqliteStore;
use serde_json::{Value, json};

use crate::Result;
use crate::permissions::{Verdict, allow};

pub(super) fn install(kit: &RoomKit<SqliteStore>) -> Result<()> {
    kit.hook(
        HookTrigger::OnToolCall,
        |event: ToolCallEvent, _: RoomContext| async move { rewritten(event) },
        HookOptions {
            name: "code-model-results".into(),
            timeout: crate::timeouts::HOOKS,
            ..HookOptions::default()
        },
    )?;
    Ok(())
}

/// `event` with what the model reads of a tool's envelope as its result and
/// the data as the structured copy. An MCP server's result is its own,
/// whatever its shape.
fn rewritten(mut event: ToolCallEvent) -> Verdict {
    if event.name.starts_with("mcp__") {
        return allow();
    }
    let Some((model, data)) = event.result.as_ref().and_then(split) else {
        return allow();
    };
    event.structured_content = Some(json!({ "data": data }));
    event.result = Some(model);
    HookResult::modify(event, Default::default())
}

/// What the model reads of `result`, a tool's envelope, and its data: none
/// for any other shape. Nested project instructions stay a field of their
/// own, that a file's content cannot imitate as it could a line of text, and
/// lead: the preview of an evicted result keeps the start of its one line.
fn split(result: &Value) -> Option<(Value, Value)> {
    let fields = result.as_object()?;
    let text = fields.get("text")?.as_str()?;
    let data = fields.get("data")?.clone();
    match (fields.len(), fields.get("project_instructions")) {
        (2, None) => Some((json!(text), data)),
        (3, Some(rules)) => Some((json!({ "project_instructions": rules, "text": text }), data)),
        _ => None,
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "a broken fixture must fail the test")]
mod tests {
    use roomkit::model::{ChannelId, ChannelType, HookAction};
    use serde_json::json;

    use super::*;

    fn served(name: &str, result: Value) -> ToolCallEvent {
        let mut event = ToolCallEvent::new(
            ChannelId::new("code").unwrap(),
            ChannelType::AI,
            "call-1",
            name,
            Default::default(),
        );
        event.result = Some(result);
        event
    }

    #[test]
    fn the_model_reads_the_text_and_the_views_keep_the_data() {
        let verdict = rewritten(served(
            "read",
            json!({"data": {"lines": []}, "text": "1: a\n"}),
        ));
        assert_eq!(verdict.action, HookAction::Modify);
        let event = verdict.event.unwrap();
        assert_eq!(event.result, Some(json!("1: a\n")));
        assert_eq!(
            event.structured_content,
            Some(json!({"data": {"lines": []}}))
        );
    }

    #[test]
    fn nested_project_instructions_stay_a_field_a_file_cannot_imitate() {
        let result = json!({"data": 1, "text": "1: a\n", "project_instructions": "rules"});
        let event = rewritten(served("read", result)).event.unwrap();
        assert_eq!(
            event.result,
            Some(json!({"text": "1: a\n", "project_instructions": "rules"}))
        );
        assert_eq!(event.structured_content, Some(json!({"data": 1})));
        let written = event.result.unwrap().to_string();
        assert!(
            written.starts_with(r#"{"project_instructions""#),
            "{written}"
        );
    }

    #[test]
    fn any_other_result_is_left_as_it_is() {
        for (name, result) in [
            ("read", json!("plain text")),
            ("read", json!({"data": 1})),
            ("read", json!({"data": 1, "text": 2})),
            ("read", json!({"data": 1, "text": "t", "extra": true})),
            ("mcp__server__read", json!({"data": 1, "text": "t"})),
        ] {
            let verdict = rewritten(served(name, result.clone()));
            assert_eq!(verdict.action, HookAction::Allow, "{name} {result}");
        }
    }
}
