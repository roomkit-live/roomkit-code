//! A fixed Cargo invocation over a supervised shell on the worker.
use super::revision::TestCheckout;
use roomkit::model::Metadata;
use roomkit_ai::{AiTool, Tool, ToolError, ToolFuture, ToolResult};
use roomkit_tools_process::{CommandEnv, OutputStore, ShellLimits, ShellTool};
use roomkit_tools_roomkit::Shell;
use serde::Deserialize;
use serde_json::json;
use std::{path::PathBuf, time::Instant};

pub(crate) const CARGO_TEST: &str = "cargo_test";
const COMMAND: &str = "cargo test --manifest-path Cargo.toml";

#[derive(Clone)]
pub(crate) struct CargoTest {
    pub(crate) workspace: PathBuf,
    pub(crate) shell: Shell,
    pub(crate) env: CommandEnv,
    pub(crate) outputs: OutputStore,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    cwd: Option<String>,
    commit: Option<String>,
}

impl Tool for CargoTest {
    fn definition(&self) -> AiTool {
        AiTool {
            name: CARGO_TEST.into(),
            description: "Run cargo test on this worker. Supply the full commit SHA to test a private checkout. Report its verified commit, duration and Cargo exit status. A timeout or a nonzero exit is a failed test run.".into(),
            parameters: json!({
                "type":"object", "additionalProperties":false,
                "properties":{
                    "cwd":{"type":"string","description":"Workspace-relative crate directory. Defaults to the workspace root."},
                    "commit":{"type":"string","description":"Full SHA of a commit available in this worker's local Git repository. For a context.executor.test_commit assignment pass that exact SHA. Only this test call can access its checkout."}
                }
            }),
            changes_state: true,
            tags: Vec::new(),
            defer_loading: false,
        }
    }

    fn handler(&self, name: String, arguments: Metadata) -> ToolFuture {
        if name != CARGO_TEST {
            return Box::pin(async move { Err(ToolError::unserved(name)) });
        }
        let runner = self.clone();
        Box::pin(async move {
            if arguments.values().any(|v| !v.is_string()) {
                return Err(ToolError::refused("cargo_test arguments must be strings"));
            }
            let input: Input = serde_json::from_value(arguments.into())
                .map_err(|e| ToolError::refused(e.to_string()))?;
            let mut args = Metadata::from_iter([
                ("command".into(), COMMAND.into()),
                ("timeout_ms".into(), 120_000.into()),
            ]);
            if let Some(cwd) = input.cwd {
                args.insert("cwd".into(), cwd.into());
            }
            match input.commit {
                Some(sha) => runner.at_commit(&sha, args).await,
                None => runner.shell.handler("shell".into(), args).await,
            }
        })
    }
}

impl CargoTest {
    async fn at_commit(&self, sha: &str, args: Metadata) -> Result<ToolResult, ToolError> {
        let checkout = TestCheckout::open(&self.workspace, sha)
            .await
            .map_err(ToolError::refused)?;
        let report = self.run_tests(&checkout, args).await;
        checkout.close().await.map_err(ToolError::failed)?;
        report
    }

    async fn run_tests(
        &self,
        checkout: &TestCheckout,
        args: Metadata,
    ) -> Result<ToolResult, ToolError> {
        let shell = ShellTool::new(&checkout.root)
            .await
            .and_then(|s| s.with_env(self.env.clone()))
            .and_then(|s| {
                s.with_limits(ShellLimits {
                    max_output_bytes: 16 * 1024,
                    ..Default::default()
                })
            })
            .map_err(|e| ToolError::failed(e.to_string()))?
            .with_output_store(self.outputs.clone());
        let start = Instant::now();
        let result = Shell::new(shell).handler("shell".into(), args).await?;
        let ToolResult::Text(report) = result else {
            return Err(ToolError::failed("Expected Cargo command output"));
        };
        let duration_ms = start.elapsed().as_millis() as u64;
        let platform = format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH);
        if let Some(copy) = roomkit_ai::current_tool_call().and_then(|c| c.structured_content.get())
        {
            copy.insert("commit", checkout.head.clone().into());
            copy.insert("platform", platform.clone().into());
            copy.insert("duration_ms", duration_ms.into());
        }
        Ok(ToolResult::Text(format!(
            "commit={}\nplatform={platform}\nduration_ms={duration_ms}\n{report}",
            checkout.head
        )))
    }
}
