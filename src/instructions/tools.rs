use super::*;
use roomkit::model::ToolCall;
use roomkit_ai::{ToolContext, ToolDefinition, ToolError, ToolOutput, Toolset};

/// Preserve existing tool schemas/results while supplying their scoped rules.
pub(crate) struct Scoped<T> {
    inner: T,
    instructions: Arc<Instructions>,
}

impl Instructions {
    pub fn wrap<T: Toolset>(self: &Arc<Self>, inner: T) -> impl Toolset {
        Scoped {
            inner,
            instructions: self.clone(),
        }
    }
}

impl<T: Toolset> Toolset for Scoped<T> {
    fn tools(&self) -> Vec<ToolDefinition> {
        self.inner.tools()
    }

    async fn call(&self, call: ToolCall, context: ToolContext) -> Result<ToolOutput, ToolError> {
        let targets: Vec<String> = match call.name.as_str() {
            "read" | "code_outline" | "list" | "grep" | "glob" => vec![
                call.arguments
                    .get("path")
                    .and_then(|p| p.as_str())
                    .unwrap_or(".")
                    .into(),
            ],
            "diff" => ["old_path", "new_path"]
                .iter()
                .filter_map(|key| {
                    call.arguments
                        .get(*key)
                        .and_then(|p| p.as_str())
                        .map(str::to_owned)
                })
                .collect(),
            "patch"
                if call
                    .arguments
                    .get("dry_run")
                    .is_none_or(|value| value == true) =>
            {
                call.arguments
                    .get("changes")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                    .flat_map(|change| {
                        ["path", "from", "to"].into_iter().filter_map(move |key| {
                            change.get(key).and_then(|v| v.as_str()).map(str::to_owned)
                        })
                    })
                    .collect()
            }
            _ => vec![],
        };
        let room = context.room_id.to_string();
        let agent = context.channel_id.to_string();
        let result = self.inner.call(call, context).await?;
        if targets.is_empty() {
            return Ok(result);
        }
        let report = self.instructions.load(targets, false).await;
        if report.checked_scopes.is_empty() && report.problems.is_empty() {
            return Ok(result);
        }
        if !self.instructions.emit(&room, &agent, &report) {
            return Ok(result);
        }
        let guidance = report.scoped();
        match result {
            ToolOutput::Json(serde_json::Value::Object(mut object)) => {
                object.insert("project_instructions".into(), guidance.into());
                Ok(ToolOutput::Json(object.into()))
            }
            ToolOutput::Text(text) => Ok(ToolOutput::Text(format!("{text}\n\n{guidance}"))),
            other => Ok(other),
        }
    }

    async fn close(&self) {
        self.inner.close().await;
    }
}
