//! Discovery and explicit per-project trust; configuration never grants itself trust.
use roomkit_hooks::{CommandHook, Configuration, Event, HookGroup};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(super) struct Entry {
    pub id: String,
    pub event: Event,
    pub group: HookGroup,
    pub hook: CommandHook,
    pub source: PathBuf,
}
impl Entry {
    pub fn label(&self) -> &str {
        self.hook
            .status_message
            .as_deref()
            .unwrap_or(self.event.name())
    }
}

pub(super) struct Catalogue {
    pub entries: Vec<Entry>,
    pub problems: Vec<String>,
    pub paths: [PathBuf; 2],
    trust: PathBuf,
    pub enabled: BTreeSet<String>,
}

fn read(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if meta.file_type().is_file() && meta.len() <= limit => {}
        _ => return Err("Expected a regular hook file within its size limit".into()),
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|_| "Cannot read hook file")?;
    if bytes.len() as u64 > limit {
        return Err("Hook file grew beyond its size limit".into());
    }
    Ok(Some(bytes))
}

impl Catalogue {
    pub fn load(workspace: &Path, state: &Path, private: &Path) -> Self {
        let mut catalogue = Self {
            entries: vec![],
            problems: vec![],
            paths: [
                state.join("hooks.json"),
                workspace.join(".roomkit/hooks.json"),
            ],
            trust: private.join("hooks-trust.json"),
            enabled: BTreeSet::new(),
        };
        catalogue.reload();
        catalogue
    }
    pub fn reload(&mut self) {
        self.entries.clear();
        self.problems.clear();
        self.enabled.clear();
        match read(&self.trust, 131_072) {
            Ok(None) => {}
            Ok(Some(bytes)) => match serde_json::from_slice(&bytes) {
                Ok(value) => self.enabled = value,
                Err(_) => self
                    .problems
                    .push("Invalid hook trust file; all hooks require review".into()),
            },
            Err(error) => self.problems.push(error),
        }
        for path in &self.paths {
            let configuration =
                read(path, 131_072).and_then(|b| b.map(|b| Configuration::parse(&b)).transpose());
            match configuration {
                Ok(Some(config)) => {
                    for (event, groups) in config.hooks {
                        for group in groups {
                            for hook in &group.hooks {
                                let definition =
                                    serde_json::to_vec(&(path, event, &group.matcher, hook))
                                        .unwrap_or_default();
                                let id = format!("{:x}", Sha256::digest(definition));
                                if self.entries.iter().any(|entry| entry.id == id) {
                                    continue;
                                }
                                self.entries.push(Entry {
                                    id,
                                    event,
                                    group: group.clone(),
                                    hook: hook.clone(),
                                    source: path.clone(),
                                });
                            }
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => self.problems.push(format!("{}: {error}", path.display())),
            }
        }
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        // Review applies to the exact loaded command/options and source. Re-read
        // before granting trust so a stale panel cannot approve a changed definition.
        self.reload();
        if !self.entries.iter().any(|e| e.id == id) {
            return Err("Hook changed or disappeared; reopen /hooks".into());
        }
        let mut next = self.enabled.clone();
        if enabled {
            next.insert(id.into());
        } else {
            next.remove(id);
        }
        let parent = self
            .trust
            .parent()
            .ok_or("Missing hook storage directory")?;
        let bytes =
            serde_json::to_vec_pretty(&next).map_err(|_| "Cannot encode hook preferences")?;
        let mut file = tempfile::NamedTempFile::new_in(parent)
            .map_err(|_| "Cannot create hook preferences")?;
        file.write_all(&bytes)
            .and_then(|()| file.as_file().sync_all())
            .map_err(|_| "Cannot save hook preferences")?;
        file.persist(&self.trust)
            .map_err(|_| "Cannot replace hook preferences")?;
        self.enabled = next;
        Ok(())
    }

    pub fn selected(&self) -> Vec<Entry> {
        self.entries
            .iter()
            .filter(|entry| self.enabled.contains(&entry.id))
            .cloned()
            .collect()
    }
}
