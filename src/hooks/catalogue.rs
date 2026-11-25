//! Discovery and explicit per-project trust; configuration never grants itself trust.
use roomkit_hooks::{CommandHook, Configuration, Event, HookGroup};
use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(super) struct Entry {
    pub id: String,
    pub event: Event,
    pub group: HookGroup,
    pub hook: CommandHook,
    pub source: PathBuf,
    /// The project's scripts the command launches, each with its content's
    /// digest as reviewed: part of what `id` identifies.
    scripts: Vec<(PathBuf, String)>,
}

/// The most scripts of the project one command's review covers, and the
/// largest of them read.
const SCRIPTS: usize = 8;
const SCRIPT_BYTES: u64 = 1_048_576;

/// The programs that run the script their first plain argument names.
const INTERPRETERS: [&str; 11] = [
    "sh", "bash", "zsh", "dash", "python", "python3", "node", "deno", "bun", "ruby", "perl",
];

/// The project's scripts `command` launches, each with its content's
/// digest: in each of its simple commands, the program itself when it is a
/// file inside `workspace`, or the script an interpreter is given;
/// `$ROOMKIT_PROJECT_DIR` reads as the workspace. A review that covered the
/// command's text alone trusted `./check.sh` whatever the script became,
/// and the agent can write the project's files. The files a command reads
/// or writes are not scripts: a hook that appends to its own log must not
/// undo its review. Other programs, on the system's path or outside the
/// project, are not the project's to change.
fn scripts(command: &str, workspace: &Path) -> Vec<(PathBuf, String)> {
    let Ok(root) = workspace.canonicalize() else {
        return Vec::new();
    };
    let project = workspace.to_string_lossy();
    let resolve = |word: &str| {
        let word = word
            .trim_matches(|c| matches!(c, '"' | '\''))
            .replace("${ROOMKIT_PROJECT_DIR}", &project)
            .replace("$ROOMKIT_PROJECT_DIR", &project);
        let path = workspace.join(&word).canonicalize().ok()?;
        (!word.is_empty() && path.starts_with(&root)).then_some(path)
    };
    let mut found: Vec<(PathBuf, String)> = Vec::new();
    for simple in command.split([';', '|', '&', '(', ')', '\n']) {
        // Leading `NAME=value` words set the program's environment.
        let mut words = simple
            .split_whitespace()
            .skip_while(|word| word.contains('=') && !word.starts_with(['.', '/', '"', '\'']));
        let Some(program) = words.next() else {
            continue;
        };
        let name = program.rsplit('/').next().unwrap_or(program);
        let script = if INTERPRETERS.contains(&name) {
            words.find(|word| !word.starts_with('-')).and_then(resolve)
        } else {
            resolve(program)
        };
        let Some(path) = script else {
            continue;
        };
        if found.iter().any(|(known, _)| *known == path) {
            continue;
        }
        if let Some(digest) = digest(&path) {
            found.push((path, digest));
            if found.len() == SCRIPTS {
                break;
            }
        }
    }
    found
}

/// The digest of the regular file at `path`, within [`SCRIPT_BYTES`].
fn digest(path: &Path) -> Option<String> {
    let bytes = read(path, SCRIPT_BYTES).ok()??;
    Some(crate::digest::sha256_hex(bytes))
}

impl Entry {
    /// Whether the project's scripts the command launches still read as
    /// they were reviewed. A hook whose script changed since does not run until
    /// it is reviewed again.
    pub fn scripts_unchanged(&self) -> bool {
        self.scripts
            .iter()
            .all(|(path, reviewed)| digest(path).as_ref() == Some(reviewed))
    }

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
    workspace: PathBuf,
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
            workspace: workspace.to_owned(),
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
            workspace: self.workspace.clone(),
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
                                let scripts = scripts(&hook.command, &self.workspace);
                                let definition = serde_json::to_vec(&(
                                    path,
                                    event,
                                    &group.matcher,
                                    hook,
                                    &scripts,
                                ))
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
                                    scripts,
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
        let bytes =
            serde_json::to_vec_pretty(&next).map_err(|_| "Cannot encode hook preferences")?;
        crate::storage::write_atomic(&self.trust, &bytes)
            .map_err(|_| "Cannot save hook preferences")?;
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

    /// Review finding: trust covered the command's text, so `./check.sh`
    /// stayed trusted whatever the agent later wrote in the script.
    #[test]
    fn a_review_covers_the_project_scripts_the_command_names() -> crate::Result<()> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let private = tempfile::tempdir()?;
        std::fs::write(workspace.path().join("check.sh"), "exit 0\n")?;
        // A file the command reads or writes is no script of its own.
        std::fs::write(workspace.path().join("seen.log"), "")?;
        std::fs::write(
            state.path().join("hooks.json"),
            serde_json::json!({"hooks": {"PreToolUse": [{"hooks": [
                {"type":"command", "command":"sh \"$ROOMKIT_PROJECT_DIR/check.sh\" --strict"},
                {"type":"command", "command":"cat >/dev/null; MODE=strict ./check.sh"},
                {"type":"command", "command":"cat check.sh >> seen.log; /bin/echo unrelated"}
            ]}]}})
            .to_string(),
        )?;
        let mut catalogue = Catalogue::load(workspace.path(), state.path(), private.path());
        let reviewed: Vec<Entry> = catalogue.entries.clone();
        assert_eq!(
            reviewed.iter().map(|e| e.scripts.len()).collect::<Vec<_>>(),
            [1, 1, 0]
        );
        catalogue.set_enabled(&reviewed[0].id, true)?;
        assert!(reviewed.iter().all(Entry::scripts_unchanged));
        std::fs::write(workspace.path().join("check.sh"), "rm -rf .\n")?;
        // The entries a turn holds no longer run the script, and the
        // catalogue read anew asks for another review.
        assert!(!reviewed[0].scripts_unchanged() && !reviewed[1].scripts_unchanged());
        assert!(reviewed[2].scripts_unchanged());
        catalogue.reload();
        assert_ne!(catalogue.entries[0].id, reviewed[0].id);
        assert_eq!(catalogue.entries[2].id, reviewed[2].id);
        assert!(catalogue.selected().is_empty());
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
