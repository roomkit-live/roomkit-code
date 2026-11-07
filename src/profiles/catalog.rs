//! The agents a project can use: its own, and the global ones every project
//! shares. A global agent's provider, model, instructions and tools are shared;
//! its MCP servers and skills stay this project's choices, because servers are
//! configured per project and some skills only exist in one.
//!
//! Moves between the two files write the destination first: a failure in
//! between leaves the agent in both, and the project's copy wins.

use super::*;
use model::Extension;

/// Agents per file, and visible to one project.
pub(super) const MAX_AGENTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    /// Shared by every project.
    Global,
    /// This project only.
    Project,
}

impl Scope {
    pub(super) fn value(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

impl State {
    /// Where agent `id` is defined; the project's copy wins over a global one.
    pub(super) fn scope(&self, id: &str) -> Option<Scope> {
        if self.project.value.agents.contains_key(id) {
            Some(Scope::Project)
        } else if self.global.value.agents.contains_key(id) {
            Some(Scope::Global)
        } else {
            None
        }
    }

    /// Agent `id` as this project runs it: a project agent, or a global one with
    /// this project's MCP servers and skills.
    pub(super) fn agent(&self, id: &str) -> Option<Agent> {
        if let Some(agent) = self.project.value.agents.get(id) {
            return Some(agent.clone());
        }
        let mut agent = self.global.value.agents.get(id)?.clone();
        let extension = self.project.value.extensions.get(id).cloned();
        let extension = extension.unwrap_or_default();
        agent.skills = extension.skills;
        agent.mcp = extension.mcp;
        Some(agent)
    }

    /// Every agent this project can select with its scope, by ID.
    pub(super) fn agents(&self) -> Vec<(Agent, Scope)> {
        let mut ids: Vec<&String> = self
            .project
            .value
            .agents
            .keys()
            .chain(self.global.value.agents.keys())
            .collect();
        ids.sort();
        ids.dedup();
        ids.into_iter()
            .filter_map(|id| Some((self.agent(id)?, self.scope(id)?)))
            .collect()
    }

    /// The agent this project opens with: its own selection, otherwise the
    /// global agent used last, otherwise the first it can use, so a new
    /// project starts ready whenever an agent exists.
    pub(super) fn selected(&self) -> Option<String> {
        let own = self.project.value.selected.as_ref();
        let last = self.global.value.selected.as_ref();
        own.filter(|id| self.scope(id).is_some())
            .or_else(|| last.filter(|id| self.global.value.agents.contains_key(*id)))
            .cloned()
            .or_else(|| self.agents().into_iter().next().map(|(agent, _)| agent.id))
    }

    /// Save `agent` in `scope`, moving it out of the other one: the form's
    /// create, edit and move. A new global ID is first claimed in every project.
    pub(super) fn save_agent(&mut self, agent: Agent, scope: Scope) -> Result<(), String> {
        let saved = self.write_agent(agent, scope);
        // Even a failed move may have written one file.
        self.refresh_roster();
        saved
    }

    fn write_agent(&mut self, agent: Agent, scope: Scope) -> Result<(), String> {
        let id = agent.id.clone();
        match scope {
            Scope::Global => {
                if !self.global.value.agents.contains_key(&id) {
                    self.claim_global_id(&id)?;
                }
                self.write_global(agent)
            }
            Scope::Project => {
                self.write_project(agent)?;
                if self.global.value.agents.contains_key(&id) {
                    let mut global = self.global.value.clone();
                    global.agents.remove(&id);
                    if global.selected.as_deref() == Some(id.as_str()) {
                        global.selected = None;
                    }
                    self.global.save(global)?;
                }
                Ok(())
            }
        }
    }

    /// Save a change to agent `agent.id` where it is defined, touching no other
    /// file: effort, MCP servers and skills.
    pub(super) fn update_agent(&mut self, agent: Agent) -> Result<(), String> {
        let updated = match self.scope(&agent.id).ok_or("Unknown agent")? {
            Scope::Global => self.write_global(agent),
            Scope::Project => self.write_project(agent),
        };
        self.refresh_roster();
        updated
    }

    /// The shared definition in the global file, then this project's MCP
    /// servers and skills in its extension, replacing a project copy it moves.
    fn write_global(&mut self, agent: Agent) -> Result<(), String> {
        let id = agent.id.clone();
        let extension = Extension {
            skills: agent.skills.clone(),
            mcp: agent.mcp.clone(),
        };
        let shared = Agent {
            skills: Vec::new(),
            mcp: BTreeMap::new(),
            ..agent
        };
        let mut global = self.global.value.clone();
        if !global.agents.contains_key(&id) && global.agents.len() >= MAX_AGENTS {
            return Err("Maximum of 64 global agents reached".into());
        }
        global.agents.insert(id.clone(), shared);
        self.global.save(global)?;
        let mut project = self.project.value.clone();
        project.agents.remove(&id);
        if extension == Extension::default() {
            project.extensions.remove(&id);
        } else {
            project.extensions.insert(id, extension);
        }
        self.project.save(project)
    }

    fn write_project(&mut self, agent: Agent) -> Result<(), String> {
        let id = agent.id.clone();
        let mut project = self.project.value.clone();
        if !project.agents.contains_key(&id) && project.agents.len() >= MAX_AGENTS {
            return Err("Maximum of 64 project agents reached".into());
        }
        project.extensions.remove(&id);
        project.agents.insert(id, agent);
        self.project.save(project)
    }

    /// Open agent `id` in this project, and remember a global one for new projects.
    pub(super) fn select_agent(&mut self, id: &str) -> Result<(), String> {
        let scope = self.scope(id).ok_or("Unknown agent")?;
        let mut project = self.project.value.clone();
        project.selected = Some(id.into());
        self.project.save(project)?;
        if scope == Scope::Global && self.global.value.selected.as_deref() != Some(id) {
            let mut global = self.global.value.clone();
            global.selected = Some(id.into());
            self.global.save(global)?;
        }
        Ok(())
    }

    /// Delete agent `id` where it is defined; a global one leaves every project.
    pub(super) fn remove_agent(&mut self, id: &str, next: Option<&str>) -> Result<(), String> {
        let removed = self.delete_agent(id, next);
        self.refresh_roster();
        removed
    }

    fn delete_agent(&mut self, id: &str, next: Option<&str>) -> Result<(), String> {
        let scope = self.scope(id).ok_or("Unknown agent")?;
        let mut project = self.project.value.clone();
        project.agents.remove(id);
        project.extensions.remove(id);
        project.selected = next.map(Into::into);
        self.project.save(project)?;
        if scope == Scope::Global {
            let mut global = self.global.value.clone();
            global.agents.remove(id);
            if global.selected.as_deref() == Some(id) {
                global.selected = None;
            }
            self.global.save(global)?;
        }
        Ok(())
    }

    /// Refuse `id` as a new global agent when another project has its own
    /// agent with that ID: the form checks it before anything is attached.
    pub(super) fn check_global_id(&self, id: &str) -> Result<(), String> {
        refuse_shared(&self.other_projects()?, id)
    }

    /// Before `id` becomes a new global agent: check it again under the lease,
    /// and forget the MCP servers and skills projects kept for an earlier global
    /// agent of that ID, this one included before the global write, so a
    /// failure after it resurrects none. The global file must be as this form
    /// saw it, or `id` may now be a live global agent whose choices are not stale.
    fn claim_global_id(&mut self, id: &str) -> Result<(), String> {
        if !self.global.is_current()? {
            return Err("Profiles changed in another instance; cancel and reopen the form".into());
        }
        let mut others = self.other_projects()?;
        refuse_shared(&others, id)?;
        for other in &mut others {
            if other.value.extensions.contains_key(id) {
                let mut value = other.value.clone();
                value.extensions.remove(id);
                other.save(value)?;
            }
        }
        if self.project.value.extensions.contains_key(id) {
            let mut project = self.project.value.clone();
            project.extensions.remove(id);
            self.project.save(project)?;
        }
        Ok(())
    }

    /// IDs other projects' own agents use, for a new agent's default ID.
    pub(super) fn other_ids(&self) -> Vec<String> {
        self.other_projects()
            .unwrap_or_default()
            .into_iter()
            .flat_map(|other| other.value.agents.into_keys())
            .collect()
    }

    /// Every other project's agent file, read under the caller's edit lease. An
    /// unreadable one refuses the change rather than missing a conflict.
    fn other_projects(&self) -> Result<Vec<Document<Project>>, String> {
        let current = self.project.path();
        let root = current
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or("Invalid profile location")?;
        let entries = std::fs::read_dir(root).map_err(|_| "Cannot inspect project profiles")?;
        let mut documents = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|_| "Cannot inspect project profiles")?;
            let kind = entry
                .file_type()
                .map_err(|_| "Cannot inspect project profiles")?;
            if kind.is_symlink() {
                return Err(format!(
                    "{} is a symlink; cannot share an agent",
                    entry.path().display()
                ));
            }
            let path = entry.path().join("agents.json");
            if !kind.is_dir() || path == current || !path.exists() {
                continue;
            }
            let document = Document::<Project>::load(path.clone()).map_err(|_| {
                format!(
                    "Cannot read {}; repair it before sharing an agent",
                    path.display()
                )
            })?;
            if !matches!(document.value.schema, 4 | PROJECT_SCHEMA) {
                return Err(format!(
                    "{} has an unsupported version; cannot share an agent",
                    path.display()
                ));
            }
            documents.push(document);
        }
        Ok(documents)
    }

    /// Reread both agent files before a form or a change.
    pub(super) fn reload_agents(&mut self) -> Result<(), String> {
        let reloaded = self.project.reload().and_then(|()| self.global.reload());
        self.refresh_roster();
        reloaded?;
        if self.project.value.schema != PROJECT_SCHEMA || self.global.value.schema != PROJECT_SCHEMA
        {
            return Err("Unsupported profile file version".into());
        }
        Ok(())
    }
}

fn refuse_shared(others: &[Document<Project>], id: &str) -> Result<(), String> {
    if others
        .iter()
        .any(|other| other.value.agents.contains_key(id))
    {
        return Err(
            "Another project has its own agent with this ID; choose another ID to share it".into(),
        );
    }
    Ok(())
}
