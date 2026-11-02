use super::*;
use roomkit::model::Metadata;
use roomkit_ai::{AiTool, Tool, ToolFuture, ToolResult, current_tool_call};

/// Preserve existing tool schemas/results while supplying their scoped rules.
pub(crate) struct Scoped {
    inner: Arc<dyn Tool>,
    /// The tool `inner` serves: a call to another passes on unchanged.
    name: String,
    instructions: Arc<Instructions>,
}

impl Instructions {
    pub fn wrap(self: &Arc<Self>, inner: Arc<dyn Tool>) -> Arc<dyn Tool> {
        Arc::new(Scoped {
            name: inner.definition().name,
            inner,
            instructions: self.clone(),
        })
    }
}

/// The paths a call of `name` with `arguments` reads, whose scoped rules
/// accompany its result.
fn targets(name: &str, arguments: &Metadata) -> Vec<String> {
    match name {
        "read" | "code_outline" | "list" | "grep" | "glob" => vec![
            arguments
                .get("path")
                .and_then(|p| p.as_str())
                .unwrap_or(".")
                .into(),
        ],
        "diff" => ["old_path", "new_path"]
            .iter()
            .filter_map(|key| {
                arguments
                    .get(*key)
                    .and_then(|p| p.as_str())
                    .map(str::to_owned)
            })
            .collect(),
        "patch" if arguments.get("dry_run").is_none_or(|value| value == true) => arguments
            .get("changes")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .flat_map(|change| {
                ["path", "from", "to"].into_iter().filter_map(move |key| {
                    change.get(key).and_then(|v| v.as_str()).map(str::to_owned)
                })
            })
            .collect(),
        _ => vec![],
    }
}

impl Tool for Scoped {
    fn definition(&self) -> AiTool {
        self.inner.definition()
    }

    fn handler(&self, name: String, arguments: Metadata) -> ToolFuture {
        let caller = current_tool_call()
            .and_then(|call| Some((call.room_id?.to_string(), call.channel_id?.to_string())));
        let Some((room, agent)) = caller.filter(|_| name == self.name) else {
            return self.inner.handler(name, arguments);
        };
        let targets = targets(&name, &arguments);
        let call = self.inner.handler(name, arguments);
        let instructions = self.instructions.clone();
        Box::pin(async move {
            let result = call.await?;
            if targets.is_empty() {
                return Ok(result);
            }
            let report = instructions.load(targets, false).await;
            if report.checked_scopes.is_empty() && report.problems.is_empty() {
                return Ok(result);
            }
            if !instructions.emit(&room, &agent, &report) {
                return Ok(result);
            }
            let guidance = report.scoped();
            match result {
                ToolResult::Json(serde_json::Value::Object(mut object)) => {
                    object.insert("project_instructions".into(), guidance.into());
                    Ok(ToolResult::Json(object.into()))
                }
                ToolResult::Text(text) => Ok(ToolResult::Text(format!("{text}\n\n{guidance}"))),
                other => Ok(other),
            }
        })
    }
}
