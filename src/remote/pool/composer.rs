//! Live, scoped ways a person can contact an executor through the composer.
use super::Scheduler;
use crate::delegation::Task;
use roomkit_tui::protocol::Choice;

impl Scheduler {
    /// Replaced pools and offline workers cannot be selected through a stale form.
    pub(crate) async fn profile_choices(&self) -> Result<Vec<Choice>, String> {
        let mut result = Vec::new();
        for row in self
            .inventory
            .rows()
            .await?
            .into_iter()
            .filter(|row| row.online())
        {
            let w = row.worker;
            let base = format!("executor:{}/{}", self.scope, w.name);
            if w.profiles.is_empty() {
                result.push(
                    Choice::new(base, format!("Start task · {} · default", w.name))
                        .with_description(format!(
                            "{} · {}",
                            w.os,
                            w.model.as_deref().unwrap_or("Model not reported")
                        )),
                );
            } else {
                for p in w.profiles {
                    result.push(
                        Choice::new(
                            format!("{base}/{}", p.id),
                            format!("Start task · {} · {}", w.name, p.id),
                        )
                        .with_description(format!("{} · {} · {}", w.os, p.label, p.model)),
                    );
                }
            }
        }
        Ok(result)
    }

    pub(super) fn accepts_target(&self, target: &serde_json::Value) -> bool {
        target.get("pool").and_then(|v| v.as_str()) == Some(&self.scope)
            && target
                .get("worker")
                .and_then(|v| v.as_str())
                .is_some_and(|w| super::identifier(w).is_ok())
    }

    pub(crate) fn target_context(&self, value: &str) -> Result<roomkit::model::Metadata, String> {
        let address = value
            .strip_prefix("executor:")
            .ok_or("Invalid executor address")?;
        let (scope, rest) = address.split_once('/').ok_or("Missing pool")?;
        if scope != self.scope {
            return Err("The selected executor pool has changed".into());
        }
        let (worker, profile) = rest
            .split_once('/')
            .map_or((rest, None), |(w, p)| (w, Some(p)));
        super::identifier(worker)?;
        let mut requirements = serde_json::Map::new();
        if let Some(profile) = profile {
            requirements.insert(
                "profile".into(),
                crate::profiles::profile_id(profile)?.into(),
            );
        }
        Ok([
            ("executor".into(), requirements.into()),
            (
                "executor_target".into(),
                serde_json::json!({"pool":scope, "worker":worker}),
            ),
        ]
        .into_iter()
        .collect())
    }

    pub(crate) fn task_choice(&self, task: &Task) -> Option<Choice> {
        let placement = self.queue.placement(&task.room)?;
        Some(
            Choice::new(
                format!("task:{}", task.id),
                format!("Open task · {placement}"),
            )
            .with_description(format!("{} · {}", task.id, task.preview())),
        )
    }
}
