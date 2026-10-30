//! Present engine-native skill results without their JSON envelope.
use super::*;
use serde_json::Value;

pub(super) struct Skills;
fn result(call: &ToolCall) -> Option<Value> {
    (call.state == ToolState::Succeeded)
        .then(|| {
            call.data
                .clone()
                .or_else(|| serde_json::from_str(&call.text).ok())
        })
        .flatten()
}
impl ToolRenderer for Skills {
    fn has_expanded_details(&self, call: &ToolCall) -> bool {
        result(call).is_some_and(|value| {
            value
                .get("instructions")
                .or_else(|| value.get("content"))
                .and_then(Value::as_str)
                .is_some_and(|body| !body.is_empty())
        })
    }
    fn render(&self, call: &ToolCall) -> Vec<String> {
        self.render_styled(call, false)
            .into_iter()
            .map(|line| line.into_iter().map(|s| s.text).collect())
            .collect()
    }
    fn render_styled(&self, call: &ToolCall, expanded: bool) -> Vec<Line> {
        let Some(value) = result(call) else {
            return content::json_or_plain(&call.text);
        };
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            return vec![vec![Span::new(error, Tone::Error)]];
        }
        let name = value
            .get("name")
            .or_else(|| value.get("filename"))
            .and_then(Value::as_str)
            .unwrap_or("Skill");
        let mut lines = vec![vec![Span::new(name, Tone::Accent)]];
        if let Some(description) = value.get("description").and_then(Value::as_str) {
            lines.push(vec![Span::new(description, Tone::Muted)]);
        }
        if value.get("already_active").and_then(Value::as_bool) == Some(true) {
            lines.push(vec![Span::new(
                "Already active in this conversation",
                Tone::Muted,
            )]);
        }
        if expanded
            && let Some(body) = value
                .get("instructions")
                .or_else(|| value.get("content"))
                .and_then(Value::as_str)
        {
            lines.extend(content::plain(body));
        }
        if let Some(references) = value.get("references").and_then(Value::as_array) {
            lines.push(vec![Span::new(
                format!(
                    "References: {}",
                    references
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Tone::Muted,
            )]);
        }
        lines
    }
}
