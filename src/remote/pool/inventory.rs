//! Read-only, group-scoped worker inventory shared by the terminal and the model.
use super::registry::{Registry, Row};
use roomkit::model::Metadata;
use roomkit_ai::{AiTool, Tool, ToolError, ToolFuture, ToolResult};

pub(crate) const LIST_EXECUTORS: &str = "list_executors";

#[derive(Clone)]
pub(crate) struct Inventory(pub(super) Registry);

impl Inventory {
    pub(crate) fn new(group: &str) -> crate::Result<Self> {
        Ok(Self(Registry::new(group)?))
    }

    pub(super) async fn lines(&self) -> Result<Vec<String>, String> {
        let rows = self.0.rows().await?;
        if rows.is_empty() {
            return Ok(vec!["No workers registered.".into()]);
        }
        Ok(rows.iter().map(describe).collect())
    }
}

impl Tool for Inventory {
    fn definition(&self) -> AiTool {
        AiTool {
            name: LIST_EXECUTORS.into(),
            description: "List the worker machines registered in this executor pool, including \
                their online status, execution and host OS, configured model, resources, labels \
                and free slots. Use this to answer questions about available executors."
                .into(),
            parameters: serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            tags: Vec::new(),
            changes_state: false,
            defer_loading: false,
        }
    }

    fn handler(&self, name: String, arguments: Metadata) -> ToolFuture {
        let inventory = self.clone();
        Box::pin(async move {
            if name != LIST_EXECUTORS {
                return Err(ToolError::unserved(name));
            }
            if !arguments.is_empty() {
                return Err(ToolError::refused("list_executors takes no arguments"));
            }
            Ok(ToolResult::Text(
                inventory
                    .lines()
                    .await
                    .map_err(ToolError::failed)?
                    .join("\n"),
            ))
        })
    }
}

fn describe(row: &Row) -> String {
    let worker = &row.worker;
    let (name, os, arch) = (&worker.name, &worker.os, &worker.arch);
    let host = worker.host_os.as_deref().unwrap_or("unspecified");
    let online = if row.online() { "online" } else { "offline" };
    let free = if row.online() {
        worker.slots.saturating_sub(row.used)
    } else {
        0
    };
    let metrics = &worker.metrics;
    let busy = metrics
        .busy
        .map_or_else(|| "sampling".into(), |n| format!("{n}% busy"));
    let load = metrics
        .load
        .map_or_else(|| "unknown".into(), |n| format!("{n:.2}"));
    let labels = worker
        .labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ");
    let model = worker.model.as_deref().unwrap_or("not reported");
    format!(
        "{name} [{online}] {os}/{arch} · host {host} · model {model} · {} CPU ({busy}) · load {load} · {} MiB available · {free}/{} slots free · {labels}",
        metrics.cpus, metrics.available_mib, worker.slots
    )
}
