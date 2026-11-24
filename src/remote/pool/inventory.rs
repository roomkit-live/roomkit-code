//! Read-only, group-scoped worker inventory shared by the terminal and the model.
use super::registry::{Registry, Row};
use roomkit::model::Metadata;
use roomkit_ai::{AiTool, Tool, ToolError, ToolFuture, ToolResult};

pub(crate) const LIST_EXECUTORS: &str = "list_executors";

#[derive(Clone)]
pub(crate) struct Inventory {
    pub(super) registry: Registry,
    pub(crate) backend: roomkit::RedisTurnBackend,
}

impl Inventory {
    pub(crate) fn for_worker(settings: &crate::config::Settings) -> crate::Result<Option<Self>> {
        settings
            .serve_pool
            .as_deref()
            .map(|group| {
                Self::new(
                    group,
                    &crate::remote::address(settings.executor_url.as_deref())?,
                )
            })
            .transpose()
    }

    pub(super) fn new(group: &str, address: &str) -> crate::Result<Self> {
        Ok(Self {
            registry: Registry::new(group, address)?,
            backend: crate::remote::backend(&format!("pool:{group}"), address)?,
        })
    }

    pub(crate) async fn check(&self) -> Result<(), String> {
        self.rows().await.map(|_| ())
    }

    pub(super) async fn rows(&self) -> Result<Vec<Row>, String> {
        tokio::time::timeout(std::time::Duration::from_secs(3), self.sample())
            .await
            .map_err(|_| "Redis did not answer within three seconds")?
    }

    async fn sample(&self) -> Result<Vec<Row>, String> {
        let (executors, rows) = tokio::try_join!(
            async {
                self.backend
                    .executors()
                    .await
                    .map_err(super::registry::unavailable)
            },
            self.registry.rows(),
        )?;
        let serving: std::collections::HashSet<_> = executors
            .into_iter()
            .filter(|executor| executor.online)
            .map(|executor| executor.executor_id)
            .collect();
        Ok(rows
            .into_iter()
            .map(|mut row| {
                row.serving = serving.contains(&row.worker.instance);
                row
            })
            .collect())
    }

    async fn lines(&self) -> Result<Vec<String>, String> {
        let rows = self.rows().await?;
        if rows.is_empty() {
            return Ok(vec!["No workers registered.".into()]);
        }
        let mut lines = Vec::new();
        for row in rows {
            lines.push(describe(&row));
            if let Some(checkout) = &row.worker.checkout {
                lines.push(format!("  Git {}", checkout.display()));
            }
            if row.worker.supports_commit_tests() {
                lines.push("  Commit tests supported on profiles offering cargo_test".into());
            }
            for (i, p) in row.worker.profiles.iter().enumerate() {
                let default = if i == 0 { " (default)" } else { "" };
                lines.push(format!(
                    "  profile {}{default} · {} · model {} · tools {}",
                    p.id,
                    p.label,
                    p.model,
                    p.tools.join(", ")
                ));
            }
        }
        Ok(lines)
    }
}

impl Tool for Inventory {
    fn definition(&self) -> AiTool {
        AiTool {
            name: LIST_EXECUTORS.into(),
            description: "List the worker machines registered in this beta executor pool, including \
                their working directory, Git checkout when reported, RoomKit Code version, online status, execution and host OS, exposed profile IDs, models, tools, resources, labels \
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
    let free = row.free_slots();
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
        "{name} [{online}] {os}/{arch} · host {host} · model {model} · {} CPU ({busy}) · load {load} · {} MiB available · {free}/{} slots free · {labels}\n  workspace {} · RoomKit Code {}",
        metrics.cpus,
        metrics.available_mib,
        worker.slots,
        worker.workspace_display(),
        worker.version.as_deref().unwrap_or("Not reported")
    )
}

/// One admission snapshot per coordinator. Redis remains authoritative for slot claims.
#[derive(Default)]
pub(super) struct Snapshot {
    sample: Option<(tokio::time::Instant, Vec<Row>)>,
}

pub(super) const REFRESH: std::time::Duration = std::time::Duration::from_millis(350);

impl Snapshot {
    pub(super) async fn rows(
        &mut self,
        load: impl Future<Output = Result<Vec<Row>, String>>,
    ) -> Result<&[Row], String> {
        if self
            .sample
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() >= REFRESH)
        {
            let rows = load.await?;
            self.sample = Some((tokio::time::Instant::now(), rows));
        }
        Ok(self
            .sample
            .as_ref()
            .map_or(&[], |(_, rows)| rows.as_slice()))
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn waiting_tasks_share_one_catalogue_read_per_interval() -> Result<(), String> {
        let mut snapshot = Snapshot::default();
        let reads = std::cell::Cell::new(0);
        for _ in 0..2 {
            for _ in 0..4 {
                snapshot
                    .rows(async {
                        reads.set(reads.get() + 1);
                        Ok(Vec::new())
                    })
                    .await?;
            }
            tokio::time::advance(REFRESH).await;
        }
        assert_eq!(reads.get(), 2);
        Ok(())
    }
}
