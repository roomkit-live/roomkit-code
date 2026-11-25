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
    /// The project's scripts the command launches, as reviewed: part of
    /// what `id` identifies when there is any.
    scripts: Vec<Script>,
}

/// One project script a hook's command launches.
#[derive(Clone)]
struct Script {
    path: PathBuf,
    /// Its path within the project, as the review panel lists it.
    shown: String,
    /// Its content's digest when it was reviewed.
    digest: String,
}

/// The most scripts of the project one command's review covers, and the
/// largest of them read.
const SCRIPTS: usize = 8;
const SCRIPT_BYTES: u64 = 1_048_576;

/// The programs that run the script their first plain argument names.
const INTERPRETERS: [&str; 11] = [
    "sh", "bash", "zsh", "dash", "python", "python3", "node", "deno", "bun", "ruby", "perl",
];

/// The words that run the command that follows them.
const WRAPPERS: [&str; 7] = ["exec", "command", "nohup", "time", "env", "nice", "timeout"];

/// `word` as the shell hands it to the program: without its quotes, and
/// `$ROOMKIT_PROJECT_DIR` read as `project`.
fn unquoted(word: &str, project: &str) -> String {
    word.replace(['"', '\''], "")
        .replace("${ROOMKIT_PROJECT_DIR}", project)
        .replace("$ROOMKIT_PROJECT_DIR", project)
}

/// The words of one simple command from its program on: past the leading
/// `NAME=value` words and the wrappers that run what follows them, with
/// their options, and `timeout`'s duration.
fn program<'w, 'a>(words: &'w [&'a str]) -> &'w [&'a str] {
    let assignment = |word: &str| word.contains('=') && !word.starts_with(['.', '/', '"', '\'']);
    let mut at = 0;
    loop {
        while words.get(at).is_some_and(|word| assignment(word)) {
            at += 1;
        }
        let Some(word) = words.get(at) else {
            return &[];
        };
        if !WRAPPERS.contains(word) {
            return &words[at..];
        }
        at += 1;
        while words.get(at).is_some_and(|word| word.starts_with('-')) {
            // `nice -n 5` and `timeout -s KILL` take a value.
            let takes_value = matches!(words[at], "-n" | "-s" | "-k");
            at += 1 + usize::from(takes_value);
        }
        if *word == "timeout" {
            at += 1;
        }
    }
}

/// The project's scripts `command` launches, each with the path it is shown
/// by and its content's digest: in each of its simple commands, the
/// program itself when it is a file inside `workspace`, or the script an
/// interpreter is given, read past the words that only run what follows
/// (`exec`, `env`, `timeout 5`…) and from the directory an earlier `cd`
/// moved to; `$ROOMKIT_PROJECT_DIR` reads as the workspace. A review that
/// covered the command's text alone trusted `./check.sh` whatever the
/// script became, and the agent can write the project's files.
///
/// This reads a command's words, not the shell's grammar: a script named
/// through another variable, a function or a tool that runs others (`make`)
/// is not found, and the review panel lists what was. The files a command
/// reads or writes are not scripts: a hook that appends to its own log must
/// not undo its review.
fn scripts(command: &str, workspace: &Path) -> Vec<Script> {
    let Ok(root) = workspace.canonicalize() else {
        return Vec::new();
    };
    let project = workspace.to_string_lossy();
    let mut directory = workspace.to_owned();
    // The directory each open subshell was entered from: a `cd` inside
    // `( … )` ends with it.
    let mut entered: Vec<PathBuf> = Vec::new();
    let mut found: Vec<Script> = Vec::new();
    let mut rest = command;
    while !rest.is_empty() && found.len() < SCRIPTS {
        let end = rest.find([';', '|', '&', '(', ')', '\n']);
        let (simple, separator) = match end {
            Some(end) => (&rest[..end], rest[end..].chars().next()),
            None => (rest, None),
        };
        rest = end.map_or("", |end| &rest[end + 1..]);
        let launched = launched(simple, &mut directory, (&root, &project));
        if let Some(path) = launched.filter(|path| found.iter().all(|known| known.path != *path))
            && let Some(digest) = digest(&path)
        {
            let shown = path.strip_prefix(&root).unwrap_or(&path);
            found.push(Script {
                shown: shown.to_string_lossy().into_owned(),
                path,
                digest,
            });
        }
        match separator {
            Some('(') => entered.push(directory.clone()),
            Some(')') => directory = entered.pop().unwrap_or_else(|| workspace.to_owned()),
            _ => {}
        }
    }
    found
}

/// The project script the simple command `simple` launches from
/// `directory`, if any; a `cd` moves `directory` instead. `root` is the
/// project's resolved directory and `project` the one its variable reads.
fn launched(
    simple: &str,
    directory: &mut PathBuf,
    (root, project): (&Path, &str),
) -> Option<PathBuf> {
    let words: Vec<&str> = simple.split_whitespace().collect();
    let words = program(&words);
    let first = words.first()?;
    let inside = |word: &str, from: &Path| {
        let path = from.join(unquoted(word, project)).canonicalize().ok()?;
        path.starts_with(root).then_some(path)
    };
    if *first == "cd" {
        if let Some(moved) = words.get(1).and_then(|to| inside(to, directory)) {
            *directory = moved;
        }
        return None;
    }
    let name = first.rsplit('/').next().unwrap_or(first);
    if INTERPRETERS.contains(&name) {
        let given = words[1..].iter().find(|word| !word.starts_with('-'));
        given.and_then(|word| inside(word, directory))
    } else {
        inside(first, directory)
    }
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
            .all(|script| digest(&script.path).as_ref() == Some(&script.digest))
    }

    /// The project scripts this hook's review covers, by their path within
    /// the project.
    pub fn pinned(&self) -> Vec<&str> {
        self.scripts
            .iter()
            .map(|script| script.shown.as_str())
            .collect()
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
                                let pinned: Vec<(&str, &str)> = scripts
                                    .iter()
                                    .map(|script| (script.shown.as_str(), script.digest.as_str()))
                                    .collect();
                                let described = (path, event, &group.matcher, hook);
                                // A hook that launches no project script is
                                // identified as it was before scripts were
                                // read: the trust given it then still holds.
                                let definition = if pinned.is_empty() {
                                    serde_json::to_vec(&described)
                                } else {
                                    serde_json::to_vec(&(described, &pinned))
                                }
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
mod tests;
