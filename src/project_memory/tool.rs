//! Route local conversation lookups and backend note operations through one tool.
use super::{History, view::excerpt};
use roomkit::model::Metadata;
use roomkit_ai::{AiTool, Tool, ToolError, ToolFuture, ToolResult};
use roomkit_tools_memory::MemoryResult;
use serde_json::json;

pub(super) struct RecallTool {
    pub(super) history: Option<History>,
    pub(super) inner: roomkit_tools_roomkit::Memory,
    pub(super) allowance: usize,
}
impl Tool for RecallTool {
    fn definition(&self) -> AiTool {
        let mut tool = self.inner.definition();
        if self.history.is_some() {
            tool.description = format!(
                "Recall earlier discussions with action history. Use a topic keyword in query, then read the match with action read and its session#message in reference. {}",
                tool.description
            );
            tool.parameters["properties"]["action"]["description"] = json!(
                "history searches conversations. read takes a conversation reference or a note id. search, save and forget operate on notes."
            );
            tool.parameters["properties"]["query"]["description"] = json!(
                "For history use one or two distinctive topic words. For note search omit to list titles."
            );
            tool.parameters["properties"]["reference"] = json!({"type":["string","null"],"description":"Read nearby messages with this session#message citation from history. Omit id and query."});
            if let Some(actions) = tool.parameters["$defs"]["Action"]["enum"].as_array_mut() {
                actions.insert(0, json!("history"));
            }
        }
        tool
    }
    fn handler(&self, name: String, mut arguments: Metadata) -> ToolFuture {
        let action = arguments.get("action").and_then(|v| v.as_str());
        let read_reference =
            action == Some("read") && arguments.get("reference").is_some_and(|v| !v.is_null());
        if action == Some("history") || read_reference {
            return self.history_call(arguments);
        }
        let suggest_history = self.history.is_some() && action == Some("search");
        if arguments.remove("reference").is_some_and(|v| !v.is_null()) {
            return Box::pin(async {
                Err(ToolError::refused(
                    "Use action read with a conversation reference.",
                ))
            });
        }
        let call = self.inner.handler(name, arguments);
        let allowance = self.allowance;
        Box::pin(async move {
            let result = call.await?;
            let ToolResult::Text(ref raw) = result else {
                return Ok(result);
            };
            let notes: MemoryResult =
                serde_json::from_str(raw).map_err(|e| roomkit_ai::ToolError::raised(&e))?;
            let rows = notes
                .notes
                .into_iter()
                .map(|n| {
                    let header = format!("{} [{}] {}", n.id, n.revision, n.title);
                    match n.text {
                        Some(body) => format!("{header}\n{body}"),
                        None => header,
                    }
                })
                .collect::<Vec<_>>();
            let text = if rows.is_empty() && suggest_history {
                r#"No saved notes matched. To find a past discussion, use memory with {"action":"history","query":"<distinctive topic word>"}."#.into()
            } else if rows.is_empty() {
                "No matching project notes".into()
            } else {
                rows.join("\n")
            };
            Ok(excerpt(&text, allowance).into())
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    query: Option<String>,
    reference: Option<String>,
}

impl RecallTool {
    fn history_call(&self, arguments: Metadata) -> ToolFuture {
        let history = self.history.clone();
        let allowance = self.allowance;
        Box::pin(async move {
            let history = history
                .ok_or_else(|| ToolError::refused("Conversation history is unavailable here."))?;
            let mut fields = arguments;
            fields.remove("action");
            fields.retain(|_, value| !value.is_null());
            let lookup: Lookup = serde_json::from_value(fields.into()).map_err(|_| {
                ToolError::refused("History accepts a topic query or a session#message reference.")
            })?;
            let text = history
                .lookup(
                    lookup.query.as_deref(),
                    lookup.reference.as_deref(),
                    allowance,
                )
                .await?;
            Ok(excerpt(&text, allowance).into())
        })
    }
}
