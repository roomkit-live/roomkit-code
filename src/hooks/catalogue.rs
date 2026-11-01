//! Discovery and explicit per-project trust; configuration never grants itself trust.
use roomkit_hooks::{CommandHook, Configuration, Event, HookGroup};
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
    /// Reread the trust file and both sources. The new catalogue replaces the
    /// old one whole, so a failure midway never leaves a partial list of
    /// enabled hooks behind its lock.
    pub fn reload(&mut self) {
        self.reload_with(read);
    }

    /// `reload`, reading files through `read`.
    fn reload_with(&mut self, read: impl Fn(&Path, u64) -> Result<Option<Vec<u8>>, String>) {
        let mut next = Self {
            entries: vec![],
            problems: vec![],
            paths: self.paths.clone(),
            trust: self.trust.clone(),
            enabled: BTreeSet::new(),
        };
        match read(&self.trust, 131_072) {
            Ok(None) => {}
            Ok(Some(bytes)) => match serde_json::from_slice(&bytes) {
                Ok(value) => next.enabled = value,
                Err(_) => next
                    .problems
                    .push("Invalid hook trust file; all hooks require review".into()),
            },
            Err(error) => next.problems.push(error),
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
                                let id = crate::digest::sha256_hex(definition);
                                if next.entries.iter().any(|entry| entry.id == id) {
                                    continue;
                                }
                                next.entries.push(Entry {
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
                Err(error) => next.problems.push(format!("{}: {error}", path.display())),
            }
        }
        *self = next;
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        let _lease = crate::storage::FileLease::exclusive_wait(
            crate::storage::private_file(&self.trust.with_extension("lock"))
                .map_err(|_| "Cannot open hook preferences lock")?,
        )
        .map_err(|_| "Hook preferences are being edited elsewhere; retry shortly")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::panic, reason = "The reader fails midway on purpose")]
    fn a_reload_that_fails_midway_keeps_the_previous_catalogue() -> crate::Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let private = tempfile::tempdir()?;
        std::fs::write(
            state.path().join("hooks.json"),
            serde_json::json!({"hooks": {"PreToolUse": [{"hooks": [
                {"type":"command", "command":"exit 2"}
            ]}]}})
            .to_string(),
        )?;
        let mut catalogue = Catalogue::load(workspace.path(), state.path(), private.path());
        let id = catalogue.entries[0].id.clone();
        catalogue.set_enabled(&id, true)?;
        // The trust file reads; the first source fails.
        let trust = catalogue.trust.clone();
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            catalogue.reload_with(|path, limit| {
                if path == trust {
                    read(path, limit)
                } else {
                    panic!("source unreadable")
                }
            });
        }));
        assert!(failed.is_err());
        assert_eq!(catalogue.entries.len(), 1);
        assert!(catalogue.enabled.contains(&id));
        assert!(catalogue.problems.is_empty());
        Ok(())
    }

    #[test]
    fn every_unreadable_file_is_reported_after_a_reload() -> crate::Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let private = tempfile::tempdir()?;
        // A damaged trust file disables every hook: the user must be told.
        std::fs::write(private.path().join("hooks-trust.json"), "{not json")?;
        let catalogue = Catalogue::load(workspace.path(), state.path(), private.path());
        assert_eq!(
            catalogue.problems,
            ["Invalid hook trust file; all hooks require review"]
        );
        std::fs::remove_file(private.path().join("hooks-trust.json"))?;
        std::fs::create_dir(private.path().join("hooks-trust.json"))?;
        std::fs::write(state.path().join("hooks.json"), "{not json")?;
        let catalogue = Catalogue::load(workspace.path(), state.path(), private.path());
        assert_eq!(catalogue.problems.len(), 2, "{:?}", catalogue.problems);
        Ok(())
    }

    #[test]
    fn simultaneous_trust_edits_preserve_both_choices() -> crate::Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let private = tempfile::tempdir()?;
        std::fs::write(
            state.path().join("hooks.json"),
            serde_json::json!({"hooks": {
                "TurnStart": [{"hooks": [
                    {"type":"command", "command":"echo first"},
                    {"type":"command", "command":"echo second"}
                ]}]
            }})
            .to_string(),
        )?;
        let one = Catalogue::load(workspace.path(), state.path(), private.path());
        let two = Catalogue::load(workspace.path(), state.path(), private.path());
        assert_eq!(one.entries.len(), 2);
        let first = one.entries[0].id.clone();
        let second = two.entries[1].id.clone();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|threads| {
            let jobs = [(one, first.clone()), (two, second.clone())]
                .into_iter()
                .map(|(mut catalogue, id)| {
                    let barrier = &barrier;
                    threads.spawn(move || {
                        barrier.wait();
                        catalogue.set_enabled(&id, true)
                    })
                })
                .collect::<Vec<_>>();
            for job in jobs {
                assert!(matches!(job.join(), Ok(Ok(()))));
            }
        });
        let actual = Catalogue::load(workspace.path(), state.path(), private.path());
        assert!(actual.enabled.contains(&first) && actual.enabled.contains(&second));
        Ok(())
    }
}
