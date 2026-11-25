//! Application discovery roots and per-agent selection; parsing and activation
//! belong to roomkit-ai. No skill is made available without a host selection.
mod action;

use super::*;
use action::Action;
use roomkit_ai::{CopyOptions, SkillMetadata, SkillRegistry};
use roomkit_tui::protocol::{Choice, Event, PanelSpec};
use roomkit_tui_engine::CommandResult;
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 256;

/// Other assistants' fields saying who may start a skill. The engine keeps
/// them as plain metadata and nothing here enforces them, so a skill
/// declaring one is refused rather than loaded without its restriction.
const INVOCATION_FIELDS: [&str; 4] = [
    "disable-model-invocation",
    "user-invocable",
    "context",
    "agent",
];

/// The description of the skill the engine registered from `path`, whose
/// directory is named `directory`, or why the application does not take it.
fn accepted(skill: SkillMetadata, path: &Path, directory: &str) -> Result<String, String> {
    if skill.name != directory {
        return Err(
            "Skill name must match the directory entry; renamed aliases are not supported".into(),
        );
    }
    let manifest = std::fs::read_to_string(path.join("SKILL.md"))
        .map_err(|_| "The skill's manifest cannot be read again")?;
    if let Some(field) = invocation_field(&manifest) {
        return Err(format!(
            "The frontmatter declares an invocation policy this application does not enforce: {field}"
        ));
    }
    Ok(skill.description)
}

/// The first of [`INVOCATION_FIELDS`] the frontmatter of `manifest` declares,
/// whatever its value: the engine's metadata leaves out a key with no value
/// or with one it does not read whole.
fn invocation_field(manifest: &str) -> Option<&'static str> {
    let mut lines = manifest.lines().map(str::trim_end);
    if lines.next() != Some("---") {
        return None;
    }
    lines.take_while(|line| *line != "---").find_map(|line| {
        let (key, _) = line.split_once(':')?;
        INVOCATION_FIELDS.into_iter().find(|field| *field == key)
    })
}

#[derive(Clone)]
struct Entry {
    source: &'static str,
    path: PathBuf,
    description: String,
    error: Option<String>,
}

#[derive(Clone, Default)]
pub(super) struct Catalogue {
    registry: SkillRegistry,
    entries: BTreeMap<String, Entry>,
    notes: Vec<String>,
    roots: Vec<PathBuf>,
}

impl Catalogue {
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub async fn load(workspace: PathBuf, state: PathBuf) -> Result<Self, String> {
        tokio::task::spawn_blocking(move || {
            let mut catalogue = Self::default();
            for (source, root) in [
                ("User", state.join("skills")),
                ("Project · .agents", workspace.join(".agents/skills")),
                ("Project · .roomkit", workspace.join(".roomkit/skills")),
            ] {
                catalogue.scan(source, &root);
                catalogue.roots.push(root);
            }
            catalogue
        })
        .await
        .map_err(|_| "Skill discovery could not finish".into())
    }

    fn scan(&mut self, source: &'static str, root: &Path) {
        let children = match std::fs::read_dir(root) {
            Ok(children) => children,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => {
                self.notes.push(format!("Cannot scan {}", root.display()));
                return;
            }
        };
        let Ok(resolved) = root.canonicalize() else {
            return;
        };
        let mut paths = Vec::new();
        for (index, child) in children.enumerate() {
            if index >= MAX_ENTRIES {
                self.notes.push(format!(
                    "{} exceeds {MAX_ENTRIES} entries; this root was not loaded",
                    root.display()
                ));
                return;
            }
            match child {
                Ok(child) => paths.push(child.path()),
                Err(_) => {
                    self.notes
                        .push(format!("Cannot inspect {}", root.display()));
                    return;
                }
            }
        }
        paths.sort();
        for path in paths {
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|s| s.to_str()).map(str::to_owned) else {
                continue;
            };
            // Hidden folders are not skill packages. Each skill is one direct child.
            if name.starts_with('.') {
                continue;
            }
            let inside = path.canonicalize().is_ok_and(|p| p.starts_with(&resolved));
            let result = if path.join("agents/openai.yaml").exists() {
                Err("Provider-specific agents/openai.yaml policies are not supported; install a reviewed portable skill without this sidecar".into())
            } else if inside {
                self.registry
                    .register(&path)
                    .map_err(|error| error.to_string())
                    .and_then(|skill| accepted(skill, &path, &name))
            } else {
                Err("Skill directory escapes its discovery root".into())
            };
            let (description, error) = match result {
                Ok(description) => (description, None),
                Err(error) => (String::new(), Some(error)),
            };
            if self.entries.contains_key(&name) {
                self.notes
                    .push(format!("{name}: {source} overrides the earlier source"));
            }
            self.entries.insert(
                name,
                Entry {
                    source,
                    path,
                    description,
                    error,
                },
            );
        }
    }

    /// The registry of the valid skills among `names`. The engine refuses a
    /// copy whose manifest passes its bound; the catalogue's own registry
    /// already stays within it, so a selection from it does too.
    pub fn selected(&self, names: &[String]) -> Result<SkillRegistry, String> {
        let names = names
            .iter()
            .filter(|name| {
                self.entries
                    .get(*name)
                    .is_some_and(|entry| entry.error.is_none())
            })
            .cloned()
            .collect::<Vec<_>>();
        self.registry
            .copy(Some(&names), CopyOptions::default())
            .map_err(|error| format!("Cannot select the agent's skills: {error}"))
    }

    fn detail(
        &self,
        current: &SessionConfig,
        name: &str,
        enabled: bool,
    ) -> Result<CommandResult, String> {
        let entry = self
            .entries
            .get(name)
            .ok_or("Unknown skill; use /skills reload to scan again")?;
        let mut body = format!(
            "{}\nSource: {}\nManifest: {}\nAvailable to this agent: {}\n\n",
            entry.description,
            entry.source,
            entry.path.join("SKILL.md").display(),
            if enabled { "yes" } else { "no" }
        );
        let mut actions = Vec::new();
        if let Some(error) = &entry.error {
            body.push_str(&format!("Unavailable: {error}\n"));
        } else if let Ok(Some(skill)) = self.registry.get_skill(name) {
            body.push_str("Instructions (preview):\n\n");
            body.extend(skill.instructions().chars().take(12_000));
            if skill.instructions().chars().count() > 12_000 {
                body.push_str("\n… Preview truncated; activation loads the complete instructions.");
            }
            if enabled {
                actions.push(ui::choice(
                    Action::Use { name, task: None }.to_string(),
                    "Use in this conversation",
                    "Ask this agent to load the skill",
                ));
            } else {
                actions.push(ui::choice(
                    Action::Enable(name).to_string(),
                    "Enable for this agent",
                    "Allow the model to select these instructions when relevant",
                ));
            }
        }
        if enabled {
            actions.push(ui::choice(
                Action::Disable(name).to_string(),
                "Disable for this agent",
                "Remove from future model requests; earlier conversation text remains",
            ));
        }
        actions.push(ui::choice(Action::List.to_string(), "Back to skills", ""));
        Ok(CommandResult::Events(vec![Event::Panel {
            conversation: current.room.as_str().into(),
            panel: PanelSpec::new(
                uuid::Uuid::new_v4().to_string(),
                "skills",
                format!("Skill · {name}"),
                body,
            )
            .with_actions(actions)
            .with_dismiss(Action::Close.to_string()),
        }]))
    }
}

/// `/skills`: the skills discovered, each with whether `agent` has it, the
/// ones it selected that are no longer found, and the catalogue's actions.
fn list(catalogue: &Catalogue, current: &SessionConfig, agent: &model::Agent) -> CommandResult {
    let mut choices = vec![
        ui::choice(
            Action::Locations.to_string(),
            "Skill directories and diagnostics",
            "Where to install skills; precedence and loading errors",
        ),
        ui::choice(
            Action::Reload.to_string(),
            "Reload skills",
            "Reread files and refresh the current agent",
        ),
    ];
    for (name, entry) in &catalogue.entries {
        let status = if entry.error.is_some() {
            "unavailable"
        } else if agent.skills.contains(name) {
            "enabled"
        } else {
            "disabled"
        };
        choices.push(
            Choice::new(Action::Open(name).to_string(), format!("{name} · {status}"))
                .with_description(format!("{} · {}", entry.source, entry.description)),
        );
    }
    let missing = agent
        .skills
        .iter()
        .filter(|name| !catalogue.entries.contains_key(*name));
    for name in missing {
        choices.push(ui::choice(
            Action::Disable(name).to_string(),
            format!("{name} · missing (remove)"),
            "No longer found in the skill directories",
        ));
    }
    let title = format!(
        "Skills for {} · {} discovered",
        agent.label,
        catalogue.entries.len()
    );
    ui::choices(current, "skills", &title, choices)
}

/// `/skills locations`: where skills are read from, in order, and what
/// reading them reported.
fn locations(catalogue: &Catalogue, current: &SessionConfig) -> CommandResult {
    let mut body =
        "Install each skill as <directory>/<name>/SKILL.md.\nPrecedence (last wins):\n".to_owned();
    for root in &catalogue.roots {
        body.push_str(&format!("  {}\n", root.display()));
    }
    body.push_str("\nSkills start disabled. Inspect and enable them per agent.\nOnly metadata is advertised until the model calls activate_skill.\nReferences use read_skill_reference; scripts are not automatically executed.\nSkills never override Ask/Plan/Yolo permissions. Provider-specific skill sidecars are not supported.\nReload after changing files. Existing active instructions use the new version after reload/restart.\n\n");
    body.push_str(&catalogue.notes.join("\n"));
    CommandResult::Events(vec![Event::Panel {
        conversation: current.room.as_str().into(),
        panel: PanelSpec::new(
            uuid::Uuid::new_v4().to_string(),
            "skills",
            "Skill discovery",
            body,
        )
        .with_actions(vec![ui::choice(
            Action::List.to_string(),
            "Back to skills",
            "",
        )])
        .with_dismiss(Action::Close.to_string()),
    }])
}

impl Profiles {
    pub async fn skills(
        &self,
        kit: &RoomKit<SqliteStore>,
        current: SessionConfig,
        arguments: &str,
    ) -> Result<CommandResult, String> {
        let action = Action::parse(arguments)?;
        let mut state = self.state.lock().await;
        // Only what writes the profile files or replaces the skills read
        // holds the cross-process edit lease.
        let _lease = action
            .mutates()
            .then(|| store::edit_lease(&self.state_dir))
            .transpose()?;
        // Preserve revisions of already issued forms; a skill edit invalidates them.
        state.pending = None;
        state.pending_delete = None;
        state.reload_agents()?;
        state.providers.reload()?;
        if state.providers.value.schema != 1 {
            return Err("Unsupported profile file version".into());
        }
        let mut agent = state
            .agent(current.agent.as_str())
            .ok_or("Configure an agent with /agents first")?
            .clone();
        let name = match action {
            Action::Close => return Ok(CommandResult::Events(Vec::new())),
            Action::Reload => return self.reload_skills(kit, &mut state, current, &agent).await,
            Action::Use { name, task } => return self.use_skill(&agent, name, task).await,
            Action::List => {
                let catalogue = self.skill_catalogue.read().await;
                return Ok(list(&catalogue, &current, &agent));
            }
            Action::Locations => {
                let catalogue = self.skill_catalogue.read().await;
                return Ok(locations(&catalogue, &current));
            }
            Action::Enable(name) | Action::Disable(name) => {
                let enable = matches!(action, Action::Enable(_));
                self.set_skill(kit, &mut state, &mut agent, name, enable)
                    .await?;
                name
            }
            Action::Open(name) => name,
        };
        let catalogue = self.skill_catalogue.read().await;
        if !catalogue.entries.contains_key(name) && matches!(action, Action::Disable(_)) {
            return Ok(CommandResult::Events(vec![Event::Notice {
                conversation: current.room.as_str().into(),
                text: format!("Removed missing skill {name} from this agent"),
            }]));
        }
        catalogue.detail(&current, name, agent.skills.iter().any(|n| n == name))
    }

    /// `/skills reload`: reads the skill directories again and assembles
    /// the agents anew on what they now hold. A catalogue the current agent
    /// cannot be assembled on is not kept.
    async fn reload_skills(
        &self,
        kit: &RoomKit<SqliteStore>,
        state: &mut State,
        mut current: SessionConfig,
        agent: &model::Agent,
    ) -> Result<CommandResult, String> {
        let next = Catalogue::load(self.workspace.clone(), self.state_dir.clone()).await?;
        let previous = std::mem::replace(&mut *self.skill_catalogue.write().await, next);
        let provider = state
            .providers
            .value
            .profiles
            .get(&agent.provider)
            .ok_or("Unknown provider")?;
        let (runtime, tools) = match self.assemble(agent, provider).await {
            Ok(value) => value,
            Err(error) => {
                *self.skill_catalogue.write().await = previous;
                return Err(error);
            }
        };
        self.installed(kit, state, &agent.id, runtime, tools)
            .await?;
        // The other agents hold the skills as they were read before.
        self.reinstall_others(kit, state, &agent.id, |other| !other.skills.is_empty())
            .await;
        state.view(&mut current);
        Ok(CommandResult::Switch(Box::new(current)))
    }

    /// `/skills enable <name>` and `disable <name>`: gives `agent` the
    /// skill, or takes it away, and installs the agent anew.
    async fn set_skill(
        &self,
        kit: &RoomKit<SqliteStore>,
        state: &mut State,
        agent: &mut model::Agent,
        name: &str,
        enable: bool,
    ) -> Result<(), String> {
        if enable {
            let catalogue = self.skill_catalogue.read().await;
            let entry = catalogue.entries.get(name).ok_or("Unknown skill")?;
            if entry.error.is_some() {
                return Err("This skill is unavailable; inspect it with /skills".into());
            }
            if !agent.skills.iter().any(|n| n == name) {
                agent.skills.push(name.into());
            }
        } else {
            agent.skills.retain(|n| n != name);
        }
        agent.validate()?;
        let provider = state
            .providers
            .value
            .profiles
            .get(&agent.provider)
            .ok_or("Unknown provider")?;
        let (runtime, tools) = self.assemble(agent, provider).await?;
        state.update_agent(agent.clone())?;
        self.installed(kit, state, &agent.id, runtime, tools).await
    }

    /// `/skills use <name> <task>`: the message that has `agent` load the
    /// skill, which it must have enabled, then do `task`.
    async fn use_skill(
        &self,
        agent: &model::Agent,
        name: &str,
        task: Option<&str>,
    ) -> Result<CommandResult, String> {
        let task = task.unwrap_or("Load the skill and explain briefly how you can help with it.");
        let catalogue = self.skill_catalogue.read().await;
        let found = catalogue.selected(&[name.into()]);
        if !agent.skills.iter().any(|n| n == name)
            || !found.is_ok_and(|selected| selected.has_entries())
        {
            return Err("Enable this skill for the current agent with /skills first".into());
        }
        Ok(CommandResult::Submit(format!(
            "Use the {name} skill. Call activate_skill with name {name:?} before applying its instructions.\n\n{task}"
        )))
    }
}

#[cfg(test)]
mod tests;
