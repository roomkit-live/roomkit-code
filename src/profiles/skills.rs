//! Application discovery roots and per-agent selection; parsing and activation
//! belong to roomkit-ai. No skill is made available without a host selection.
use super::*;
use roomkit_ai::SkillRegistry;
use roomkit_cli_roomkit::CommandResult;
use roomkit_tui::protocol::{Choice, Event, PanelSpec};
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 256;

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
                    .and_then(|skill| {
                        if skill.name() == name { Ok(skill.description().to_owned()) }
                        else { Err("Skill name must match the directory entry; renamed aliases are not supported".into()) }
                    })
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

    pub fn selected(&self, names: &[String]) -> SkillRegistry {
        let names = names
            .iter()
            .filter(|name| {
                self.entries
                    .get(*name)
                    .is_some_and(|entry| entry.error.is_none())
            })
            .cloned()
            .collect::<Vec<_>>();
        self.registry.copy(Some(&names))
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
        } else if let Some(skill) = self.registry.get(name) {
            body.push_str("Instructions (preview):\n\n");
            body.extend(skill.instructions().chars().take(12_000));
            if skill.instructions().chars().count() > 12_000 {
                body.push_str("\n… Preview truncated; activation loads the complete instructions.");
            }
            if enabled {
                actions.push(ui::choice(
                    format!("use:{name}"),
                    "Use in this conversation",
                    "Ask this agent to load the skill",
                ));
            } else {
                actions.push(ui::choice(
                    format!("enable:{name}"),
                    "Enable for this agent",
                    "Allow the model to select these instructions when relevant",
                ));
            }
        }
        if enabled {
            actions.push(ui::choice(
                format!("disable:{name}"),
                "Disable for this agent",
                "Remove from future model requests; earlier conversation text remains",
            ));
        }
        actions.push(ui::choice("list", "Back to skills", ""));
        Ok(CommandResult::Events(vec![Event::Panel {
            conversation: current.room.as_str().into(),
            panel: PanelSpec {
                id: uuid::Uuid::new_v4().to_string(),
                command: "skills".into(),
                title: format!("Skill · {name}"),
                body,
                actions,
                dismiss: "close".into(),
            },
        }]))
    }
}

impl Profiles {
    pub async fn skills(
        &self,
        kit: &RoomKit<SqliteStore>,
        current: SessionConfig,
        arguments: &str,
    ) -> Result<CommandResult, String> {
        let mut state = self.state.lock().await;
        let _lease = store::edit_lease(&self.state_dir)?;
        // Preserve revisions of already issued forms; a skill edit invalidates them.
        state.pending = None;
        state.pending_delete = None;
        state.project.reload()?;
        state.providers.reload()?;
        if state.providers.value.schema != 1 || state.project.value.schema != 2 {
            return Err("Unsupported profile file version".into());
        }
        let mut agent = state
            .project
            .value
            .agents
            .get(current.agent.as_str())
            .ok_or("Configure an agent with /agents first")?
            .clone();
        let arguments = arguments.trim();
        if arguments == "close" {
            return Ok(CommandResult::Events(Vec::new()));
        }
        if arguments == "reload" {
            let next = Catalogue::load(self.workspace.clone(), self.state_dir.clone()).await?;
            let previous = std::mem::replace(&mut *self.skill_catalogue.write().await, next);
            let provider = state
                .providers
                .value
                .profiles
                .get(&agent.provider)
                .ok_or("Unknown provider")?;
            let assembled = self.assemble(&agent, provider).await;
            let (runtime, tools) = match assembled {
                Ok(value) => value,
                Err(error) => {
                    *self.skill_catalogue.write().await = previous;
                    return Err(error);
                }
            };
            state.install(kit, &agent.id, runtime, tools)?;
            let mut current = current;
            state.view(&mut current);
            return Ok(CommandResult::Switch(Box::new(current)));
        }
        let (action, name) = arguments
            .split_once([':', ' '])
            .unwrap_or(("open", arguments));
        if matches!(action, "enable" | "disable") {
            if action == "enable" {
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
            let (runtime, tools) = self.assemble(&agent, provider).await?;
            let mut project = state.project.value.clone();
            project.agents.insert(agent.id.clone(), agent.clone());
            state.project.save(project)?;
            state.install(kit, &agent.id, runtime, tools)?;
        } else if action == "use" {
            let (name, task) = name.split_once(' ').unwrap_or((
                name,
                "Load the skill and explain briefly how you can help with it.",
            ));
            if !agent.skills.iter().any(|n| n == name)
                || self
                    .skill_catalogue
                    .read()
                    .await
                    .selected(&[name.into()])
                    .is_empty()
            {
                return Err("Enable this skill for the current agent with /skills first".into());
            }
            return Ok(CommandResult::Submit(format!(
                "Use the {name} skill. Call activate_skill with name {name:?} before applying its instructions.\n\n{task}"
            )));
        } else if !matches!(action, "open") {
            return Err(
                "Use /skills [list|reload|enable <name>|disable <name>|use <name> <task>]".into(),
            );
        }
        let catalogue = self.skill_catalogue.read().await;
        if arguments.is_empty() || arguments == "list" {
            let mut choices = vec![
                ui::choice(
                    "locations",
                    "Skill directories and diagnostics",
                    "Where to install skills; precedence and loading errors",
                ),
                ui::choice(
                    "reload",
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
                choices.push(Choice {
                    value: format!("open:{name}"),
                    label: format!("{name} · {status}"),
                    description: format!("{} · {}", entry.source, entry.description),
                });
            }
            for name in agent
                .skills
                .iter()
                .filter(|name| !catalogue.entries.contains_key(*name))
            {
                choices.push(ui::choice(
                    format!("disable:{name}"),
                    format!("{name} · missing (remove)"),
                    "No longer found in the skill directories",
                ));
            }
            return Ok(ui::choices(
                &current,
                "skills",
                &format!(
                    "Skills for {} · {} discovered",
                    agent.label,
                    catalogue.entries.len()
                ),
                choices,
            ));
        }
        if arguments == "locations" {
            let mut body =
                "Install each skill as <directory>/<name>/SKILL.md.\nPrecedence (last wins):\n"
                    .to_owned();
            for root in &catalogue.roots {
                body.push_str(&format!("  {}\n", root.display()));
            }
            body.push_str("\nSkills start disabled. Inspect and enable them per agent.\nOnly metadata is advertised until the model calls activate_skill.\nReferences use read_skill_reference; scripts are not automatically executed.\nPermissions remain Ask/Yolo. Provider-specific skill sidecars are not supported.\nReload after changing files. Existing active instructions use the new version after reload/restart.\n\n");
            body.push_str(&catalogue.notes.join("\n"));
            return Ok(CommandResult::Events(vec![Event::Panel {
                conversation: current.room.as_str().into(),
                panel: PanelSpec {
                    id: uuid::Uuid::new_v4().to_string(),
                    command: "skills".into(),
                    title: "Skill discovery".into(),
                    body,
                    actions: vec![ui::choice("list", "Back to skills", "")],
                    dismiss: "close".into(),
                },
            }]));
        }
        if !catalogue.entries.contains_key(name) && action == "disable" {
            return Ok(CommandResult::Events(vec![Event::Notice {
                text: format!("Removed missing skill {name} from this agent"),
            }]));
        }
        catalogue.detail(&current, name, agent.skills.iter().any(|n| n == name))
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "fixture failures fail the test")]
    use super::*;
    fn skill(root: &Path, name: &str, body: &str) {
        let directory = root.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Fixture skill\n---\n{body}"),
        )
        .unwrap();
    }
    #[tokio::test]
    async fn roots_have_deterministic_precedence_and_bad_skills_do_not_hide_valid_ones() {
        let workspace = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        skill(&state.path().join("skills"), "review", "User version");
        skill(
            &workspace.path().join(".agents/skills"),
            "review",
            "Project version",
        );
        skill(
            &workspace.path().join(".roomkit/skills"),
            "review",
            "Native version",
        );
        skill(&workspace.path().join(".agents/skills"), "valid", "Good");
        std::fs::create_dir_all(workspace.path().join(".agents/skills/broken")).unwrap();
        let catalogue = Catalogue::load(workspace.path().into(), state.path().into())
            .await
            .unwrap();
        assert_eq!(
            catalogue
                .selected(&["review".into()])
                .get("review")
                .unwrap()
                .instructions(),
            "Native version"
        );
        assert!(catalogue.entries["broken"].error.is_some());
        assert_eq!(
            catalogue.selected(&["valid".into(), "broken".into()]).len(),
            1
        );
        // A broken override must not silently reactivate an earlier version.
        std::fs::write(
            workspace.path().join(".roomkit/skills/review/SKILL.md"),
            "invalid",
        )
        .unwrap();
        let catalogue = Catalogue::load(workspace.path().into(), state.path().into())
            .await
            .unwrap();
        assert!(catalogue.selected(&["review".into()]).is_empty());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn discovery_refuses_escape_links_and_foreign_invocation_sidecars() {
        let workspace = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        skill(outside.path(), "linked", "Outside");
        let root = workspace.path().join(".agents/skills");
        std::fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(outside.path().join("linked"), root.join("linked")).unwrap();
        skill(&root, "policy", "Instructions");
        std::fs::create_dir_all(root.join("policy/agents")).unwrap();
        std::fs::write(
            root.join("policy/agents/openai.yaml"),
            "policy:\n  allow_implicit_invocation: false\n",
        )
        .unwrap();
        let catalogue = Catalogue::load(workspace.path().into(), state.path().into())
            .await
            .unwrap();
        assert!(
            catalogue.entries["linked"]
                .error
                .as_ref()
                .unwrap()
                .contains("escapes")
        );
        assert!(catalogue.entries["policy"].error.is_some());
        assert!(
            catalogue
                .selected(&["linked".into(), "policy".into()])
                .is_empty()
        );
    }
}
