//! Actions on Hub downloads and installed GGUFs. Model changes always require a choice.
use super::*;
use crate::local_models::{cache::Cache, hub::ModelFile};
use roomkit_tui::protocol::PanelSpec;

pub(in crate::profiles) struct Removal {
    room: String,
    agent: String,
    token: String,
    file: ModelFile,
}

impl Profiles {
    pub(crate) async fn manage_downloads(
        &self,
        kit: &RoomKit<SqliteStore>,
        current: SessionConfig,
        args: &str,
    ) -> Result<CommandResult, String> {
        let confirmation = self
            .state
            .lock()
            .await
            .pending_gguf_removal
            .take_if(|p| p.room == current.room.as_str() && p.agent == current.agent.as_str());
        let downloads = &self.local_models.downloads;
        if args == "close" {
            return Ok(CommandResult::Events(Vec::new()));
        }
        if args.is_empty() {
            return Ok(panel(&current, downloads.panel()));
        }
        let cache = Cache::locate()?;
        if args == "installed" {
            return Ok(installed(&current, &cache));
        }
        let (action, value) = args.split_once(' ').ok_or("Unknown download action")?;
        if action == "cancel" {
            downloads.cancel(value)?;
            return Ok(panel(&current, downloads.panel()));
        }
        if action == "confirm-remove" {
            let pending = confirmation
                .filter(|p| p.token == value)
                .ok_or("Open the model and confirm its removal again")?;
            self.remove_model(&cache, &pending.file).await?;
            downloads.forget(&pending.file);
            return Ok(installed(&current, &cache));
        }
        self.installed_action(kit, current, &cache, action, value)
            .await
    }

    async fn installed_action(
        &self,
        kit: &RoomKit<SqliteStore>,
        current: SessionConfig,
        cache: &Cache,
        action: &str,
        value: &str,
    ) -> Result<CommandResult, String> {
        let file = ModelFile::parse(value)
            .filter(|_| value.len() <= 256)
            .ok_or("Invalid model selection")?;
        if !cache.installed(&file) {
            return Err("This model is no longer installed. Open /downloads to refresh.".into());
        }
        match action {
            "details" => Ok(panel(&current, model_panel(&file, None))),
            "remove" => {
                let token = uuid::Uuid::new_v4().to_string();
                let result = panel(&current, model_panel(&file, Some(&token)));
                self.state.lock().await.pending_gguf_removal = Some(Removal {
                    room: current.room.to_string(),
                    agent: current.agent.to_string(),
                    file,
                    token,
                });
                Ok(result)
            }
            "use" => {
                self.local_action(kit, current, "model", &format!("- use {value}"))
                    .await
            }
            _ => Err("Unknown download action".into()),
        }
    }

    async fn remove_model(&self, cache: &Cache, file: &ModelFile) -> Result<(), String> {
        let mut state = self.state.lock().await;
        state.reload_agents()?;
        let id = file.id();
        let installed = cache.path(file).canonicalize().map_err(|e| e.to_string())?;
        let used = |model: &str| {
            model == id
                || (!model.starts_with("hf://")
                    && (model.ends_with(".gguf")
                        || model.split(':').next() == Some(file.repo.as_str()))
                    && crate::local_models::resolve(model, &self.workspace)
                        .ok()
                        .and_then(|path| PathBuf::from(path).canonicalize().ok())
                        .is_some_and(|path| path == installed))
        };
        crate::sync::blocking(|| {
            if state.agents().iter().any(|(agent, _)| used(&agent.model))
                || state.running.values().any(|run| used(&run.model))
            {
                return Err("This model is selected by an agent or conversation. Select another model first.".into());
            }
            cache.remove(file)
        })
    }
}

fn installed(current: &SessionConfig, cache: &Cache) -> CommandResult {
    let entries: Vec<_> = crate::sync::blocking(|| cache.models())
        .into_iter()
        .map(|file| {
            choice(
                format!("details {}", file.id()),
                &file.name,
                format!("{} · {}", file.repo, &file.revision[..12]),
            )
        })
        .collect();
    let title = if entries.is_empty() {
        "No installed GGUF models"
    } else {
        "Installed GGUF models"
    };
    choices(current, "downloads", title, entries)
}

fn model_panel(file: &ModelFile, confirmation: Option<&str>) -> PanelSpec {
    let id = file.id();
    let detail = format!("{}\n{} · {}", file.name, file.repo, file.revision);
    let (title, body, actions) = if let Some(token) = confirmation {
        (
            "Delete this model?",
            format!(
                "{detail}\n\nRemove this quantization and its unshared weights from the Hugging Face cache on this machine."
            ),
            vec![
                Choice::new(format!("details {id}"), "Keep model"),
                Choice::new(format!("confirm-remove {token}"), "Delete from disk"),
            ],
        )
    } else {
        (
            "Installed model",
            detail,
            vec![
                Choice::new(format!("use {id}"), "Use in this conversation"),
                Choice::new(format!("remove {id}"), "Delete from disk…"),
            ],
        )
    };
    PanelSpec::new("installed-gguf", "downloads", title, body)
        .with_actions(actions)
        .with_dismiss("close")
}

fn panel(current: &SessionConfig, panel: PanelSpec) -> CommandResult {
    CommandResult::Events(vec![Event::Panel {
        conversation: current.room.as_str().into(),
        panel,
    }])
}
