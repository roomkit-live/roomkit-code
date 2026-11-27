//! The GGUF picker for the current conversation, a new agent, or an agent being edited.
use super::*;
use crate::local_models::{
    cache::Cache,
    hub::{Hub, ModelFile, SearchScope},
};

#[derive(Clone, Debug)]
pub(in crate::profiles) enum Destination {
    Current,
    New(String),
    Edit(String),
}

impl SearchScope {
    fn search_action(self) -> &'static str {
        match self {
            Self::TextGeneration => "search",
            Self::All => "all-search",
        }
    }
    fn repo_action(self) -> &'static str {
        match self {
            Self::TextGeneration => "repo",
            Self::All => "all-repo",
        }
    }
}

impl Destination {
    fn command(&self) -> &'static str {
        match self {
            Self::Current => "model",
            Self::New(_) => "providers",
            Self::Edit(_) => "agents",
        }
    }

    fn arg(&self, verb: &str, rest: &str) -> String {
        let target = match self {
            Self::Current => "-",
            Self::New(id) | Self::Edit(id) => id,
        };
        format!("gguf {target} {verb} {rest}").trim_end().to_owned()
    }

    fn parse(command: &str, target: &str) -> Result<Self, String> {
        match command {
            "model" if target == "-" => Ok(Self::Current),
            "providers" if model::valid_id(target) => Ok(Self::New(target.into())),
            "agents" if model::valid_id(target) => Ok(Self::Edit(target.into())),
            _ => Err("Invalid local model destination".into()),
        }
    }

    fn check(&self, state: &State, current: &SessionConfig) -> Result<String, String> {
        let id = match self {
            Self::New(id) => Some(id.clone()),
            Self::Edit(id) => state.agent(id).map(|a| a.provider),
            Self::Current => state
                .running_agent(current.room.as_str(), current.agent.as_str())
                .map(|a| a.provider),
        };
        if let Some(id) = id
            && state
                .providers
                .value
                .profiles
                .get(&id)
                .is_some_and(|p| p.kind == ProviderKind::LlamaCpp)
        {
            return Ok(id);
        }
        Err("Choose a local llama.cpp connection first".into())
    }

    pub(in crate::profiles) fn menu(
        &self,
        current: &SessionConfig,
    ) -> Result<CommandResult, String> {
        let mut entries = vec![choice(
            self.arg("search", ""),
            "Search Hugging Face",
            "Text-generation models in GGUF format",
        )];
        entries.push(choice(
            self.arg("all-search", ""),
            "Search all GGUF models",
            "Include audio, other tasks and repositories without a task tag",
        ));
        if let Self::Current = self {
            entries.push(choice(
                self.arg("context", ""),
                "Agent context capacity",
                "Set the token window for this agent’s local models",
            ));
        }
        for file in crate::sync::blocking(|| Cache::locate().map(|c| c.models()))? {
            entries.push(choice(
                self.arg("use", &file.id()),
                &file.name,
                format!("{} · {} · installed", file.repo, &file.revision[..12]),
            ));
        }
        if let Self::New(id) = self {
            entries.push(choice(
                Action::NewAgentOn {
                    provider: id,
                    model: "",
                    typed: true,
                }
                .to_string(),
                "Enter a GGUF path or model ID",
                "Use a local file or repository:quantization",
            ));
        }
        Ok(choices(
            current,
            self.command(),
            "Local GGUF models",
            entries,
        ))
    }

    pub(super) async fn search(
        &self,
        current: &SessionConfig,
        query: &str,
        scope: SearchScope,
    ) -> Result<CommandResult, String> {
        let hub = Hub::public()?;
        if crate::local_models::hub::valid_repo(query) {
            return self.files(current, &hub, query, scope).await;
        }
        let repos = hub.search(query, scope).await?;
        let mut entries: Vec<_> = repos
            .into_iter()
            .map(|r| choice(self.arg(scope.repo_action(), &r), r, "Choose a GGUF file"))
            .collect();
        let title = if entries.is_empty() {
            "No matching GGUF repositories"
        } else {
            "Choose a Hugging Face repository"
        };
        entries.push(choice(
            self.arg(scope.search_action(), ""),
            "Search again",
            "Enter a model name or owner/repository",
        ));
        Ok(choices(current, self.command(), title, entries))
    }

    async fn files(
        &self,
        current: &SessionConfig,
        hub: &Hub,
        repo: &str,
        scope: SearchScope,
    ) -> Result<CommandResult, String> {
        let cache = Cache::locate()?;
        let variants = hub
            .variants(repo, None)
            .await
            .map_err(|e| format!("Could not list GGUF files in {repo}. {e}"))?;
        let mut entries = Vec::new();
        for model in variants
            .into_iter()
            .filter(|v| v.first.id().len() <= 256)
            .take(1000)
        {
            let status = if cache.installed(&model.first) {
                "installed"
            } else {
                "download in background"
            };
            entries.push(choice(
                self.arg("use", &model.first.id()),
                model.first.name,
                format!("{:.2} GB · {status}", model.size as f64 / 1e9),
            ));
        }
        let title = if entries.is_empty() {
            format!("No installable GGUF files in {repo}")
        } else {
            format!("Choose a GGUF file · {repo}")
        };
        entries.push(choice(
            self.arg(scope.search_action(), ""),
            "Search again",
            "Choose another repository",
        ));
        Ok(choices(current, self.command(), &title, entries))
    }
}

impl Profiles {
    pub(super) async fn local_action(
        &self,
        kit: &RoomKit<SqliteStore>,
        current: SessionConfig,
        command: &str,
        args: &str,
    ) -> Result<CommandResult, String> {
        let mut words = args.splitn(3, ' ');
        let dest = Destination::parse(command, words.next().unwrap_or(""))?;
        let verb = words.next().unwrap_or("");
        let value = words.next().unwrap_or("");
        let provider = {
            let mut state = self.state.lock().await;
            let provider = dest.check(&state, &current)?;
            state.pending = None;
            if verb == "context" && matches!(dest, Destination::Current) {
                return self.local_context_form(&mut state, &current);
            }
            if matches!(verb, "search" | "all-search" | "background-search") {
                let scope = if verb == "search" {
                    SearchScope::TextGeneration
                } else {
                    SearchScope::All
                };
                let form = FormSpec::new(
                    uuid::Uuid::new_v4().to_string(),
                    dest.command(),
                    "Search Hugging Face",
                    vec![field(
                        "query",
                        "Model or repository",
                        "",
                        "Leave empty for popular GGUF models",
                        false,
                        FieldKind::Text,
                    )],
                );
                return Ok(open_form(
                    &mut state,
                    &current,
                    form,
                    Target::Hub(dest, scope),
                ));
            }
            provider
        };
        match verb {
            "repo" => {
                dest.files(
                    &current,
                    &Hub::public()?,
                    value,
                    SearchScope::TextGeneration,
                )
                .await
            }
            "all-repo" | "background-repo" => {
                dest.files(&current, &Hub::public()?, value, SearchScope::All)
                    .await
            }
            "download" => self.download_local(&current, selection(value)?, Cache::locate()?),
            "use" => {
                self.select_local(kit, current, dest, &provider, value)
                    .await
            }
            _ => Err("Unknown GGUF selection".into()),
        }
    }

    fn download_local(
        &self,
        current: &SessionConfig,
        file: ModelFile,
        cache: Cache,
    ) -> Result<CommandResult, String> {
        let downloads = &self.local_models.downloads;
        if !cache.installed(&file) {
            downloads.start(cache, file)?;
        }
        Ok(CommandResult::Events(vec![Event::Panel {
            conversation: current.room.as_str().into(),
            panel: downloads.panel(),
        }]))
    }

    async fn select_local(
        &self,
        kit: &RoomKit<SqliteStore>,
        current: SessionConfig,
        dest: Destination,
        provider: &str,
        id: &str,
    ) -> Result<CommandResult, String> {
        let file = selection(id)?;
        let cache = Cache::locate()?;
        if !cache.installed(&file) {
            return self.download_local(&current, file, cache);
        }
        let mut state = self.state.lock().await;
        let _lease = matches!(dest, Destination::Current)
            .then(|| store::edit_lease(&self.state_dir))
            .transpose()?;
        state.providers.reload()?;
        state.reload_agents()?;
        if dest.check(&state, &current)? != provider {
            return Err("The agent connection changed. Choose the model again.".into());
        }
        match dest {
            Destination::Current => {
                self.change_current(kit, &mut state, current, |state, agent| {
                    self.switch(state, agent, id)
                })
                .await
            }
            Destination::New(provider) => {
                new_agent_form(&mut state, &current, (&provider, id, None), false)
            }
            Destination::Edit(agent) => {
                let mut profile = state.agent(&agent).ok_or("Unknown agent")?;
                self.switch(&state, &mut profile, id);
                edit_agent_form(&mut state, &current, Some(&profile))
            }
        }
    }
}

fn selection(id: &str) -> Result<ModelFile, String> {
    if id.len() > 256 {
        return Err("Model identifier is too long".into());
    }
    ModelFile::parse(id).ok_or_else(|| "Invalid GGUF selection".into())
}
