//! A fixed Cargo invocation over the worker's existing supervised shell.
use roomkit::model::Metadata;
use roomkit_ai::{AiTool, Tool, ToolError, ToolFuture};
use roomkit_tools_roomkit::Shell;
use serde_json::json;

pub(crate) const CARGO_TEST: &str = "cargo_test";
const COMMAND: &str = "cargo test --manifest-path Cargo.toml";

pub(crate) struct CargoTest(pub(crate) Shell);

impl Tool for CargoTest {
    fn definition(&self) -> AiTool {
        AiTool {
            name: CARGO_TEST.into(),
            description: "Run cargo test in a Rust crate on this executor. Return the exit code, stdout and stderr. A nonzero exit or a timeout is not a pass.".into(),
            parameters: json!({
                "type":"object", "additionalProperties":false,
                "properties":{"cwd":{"type":"string","description":"Workspace-relative directory containing Cargo.toml. Defaults to the workspace root."}}
            }),
            changes_state: true,
            tags: Vec::new(),
            defer_loading: false,
        }
    }

    fn handler(&self, name: String, mut arguments: Metadata) -> ToolFuture {
        if name != CARGO_TEST {
            return Box::pin(async move { Err(ToolError::unserved(name)) });
        }
        if arguments.keys().any(|key| key != "cwd")
            || arguments.get("cwd").is_some_and(|cwd| !cwd.is_string())
        {
            return Box::pin(async {
                Err(ToolError::refused(
                    "cargo_test accepts only a workspace-relative cwd",
                ))
            });
        }
        arguments.insert("command".into(), COMMAND.into());
        arguments.insert("timeout_ms".into(), 120_000.into());
        self.0.handler("shell".into(), arguments)
    }
}
