use super::*;
use rustix::{
    fd::OwnedFd,
    fs::{self, Mode, OFlags},
};
use std::{collections::BTreeSet, io::Read, path::Component};

const FILE_LIMIT: usize = 16 * 1024;
const TOTAL_LIMIT: usize = 32 * 1024;

fn directory(base: &Path) -> Result<Option<OwnedFd>, String> {
    match fs::open(
        base,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => Ok(Some(fd)),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(_) => Err(format!(
            "Cannot open instruction directory {}",
            base.display()
        )),
    }
}

fn read(dir: &OwnedFd, name: &str) -> Result<Option<String>, &'static str> {
    let fd = match fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(_) => return Err("Cannot read instructions (symlinks are not followed)"),
    };
    let mut file = std::fs::File::from(fd);
    let metadata = file
        .metadata()
        .map_err(|_| "Cannot inspect instruction file")?;
    if !metadata.is_file() {
        return Err("Instructions must be a regular file");
    }
    if metadata.len() > FILE_LIMIT as u64 {
        return Err("Instructions exceed 16 KiB; shorten or split by directory");
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(FILE_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read instructions")?;
    if bytes.len() > FILE_LIMIT {
        return Err("Instructions grew beyond 16 KiB");
    }
    let text = String::from_utf8(bytes).map_err(|_| "Instructions must be UTF-8")?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text).trim();
    Ok((!text.is_empty()).then(|| text.to_owned()))
}

impl Report {
    fn collect(&mut self, dir: &OwnedFd, path: &Path, scope: &str, seen: &mut BTreeSet<PathBuf>) {
        // A failed override is visible and never silently replaced by weaker rules.
        for name in ["AGENTS.override.md", "AGENTS.md"] {
            let path = path.join(name);
            if !seen.insert(path.clone()) {
                return;
            }
            match read(dir, name) {
                Ok(None) => continue,
                Ok(Some(text)) => {
                    let size = text.len()
                        + serde_json::json!(path.to_string_lossy()).to_string().len()
                        + serde_json::json!(scope).to_string().len()
                        + 256;
                    if self.bytes + size > TOTAL_LIMIT {
                        self.problems.push(format!(
                            "{}: instruction chain exceeds 32 KiB; file not loaded",
                            path.display()
                        ));
                    } else {
                        self.bytes += size;
                        self.sources.push(Source {
                            path,
                            scope: scope.into(),
                            revision: crate::digest::sha256_hex(text.as_bytes()),
                            text,
                        });
                    }
                    return;
                }
                Err(error) => {
                    self.problems.push(format!("{}: {error}", path.display()));
                    return;
                }
            }
        }
    }
}

impl Instructions {
    pub(super) fn discover(&self, targets: &[String], base: bool) -> Report {
        let mut report = Report::default();
        let mut seen = BTreeSet::new();
        if base {
            match directory(&self.global) {
                Ok(Some(fd)) => {
                    report.collect(&fd, &self.global, "global (this workspace)", &mut seen)
                }
                Err(e) => report.problems.push(e),
                _ => {}
            }
        }
        let root = match directory(&self.workspace) {
            Ok(Some(fd)) => fd,
            _ => {
                report
                    .problems
                    .push("Cannot open the project instruction boundary".into());
                return report;
            }
        };
        if base {
            report.collect(&root, &self.workspace, ". (whole workspace)", &mut seen);
        }
        for target in targets.iter().take(64) {
            let path = Path::new(target);
            let parts: Vec<_> = path
                .components()
                .filter(|p| !matches!(p, Component::CurDir))
                .collect();
            if target.len() > 2048
                || parts.len() > 32
                || parts.iter().any(|p| !matches!(p, Component::Normal(_)))
            {
                report.problems.push("Instruction targets must be relative project paths without '..', at most 32 components / 2048 bytes".into());
                continue;
            }
            let mut current = &root;
            let mut owned = Vec::new();
            let mut relative = PathBuf::new();
            for (index, component) in parts.iter().enumerate() {
                let Component::Normal(name) = component else {
                    continue;
                };
                let next = fs::openat(
                    current,
                    Path::new(name),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                );
                match next {
                    Ok(fd) => {
                        owned.push(fd);
                        current = &owned[owned.len() - 1];
                        relative.push(name);
                        let scope = relative.to_string_lossy().into_owned();
                        if !report.checked_scopes.contains(&scope) {
                            report.checked_scopes.push(scope);
                        }
                        report.collect(
                            current,
                            &self.workspace.join(&relative),
                            &relative.to_string_lossy(),
                            &mut seen,
                        );
                    }
                    Err(rustix::io::Errno::NOENT) => break,
                    Err(rustix::io::Errno::NOTDIR) if index + 1 == parts.len() => {
                        // A final regular file contributes only its parent scope.
                        // A symlink is not an excuse to load rules from its target.
                        if fs::statat(current, Path::new(name), fs::AtFlags::SYMLINK_NOFOLLOW)
                            .is_ok_and(|s| {
                                fs::FileType::from_raw_mode(s.st_mode) == fs::FileType::RegularFile
                            })
                        {
                            break;
                        }
                        report.problems.push(format!("{}: target is not a regular file or directory; symlinks are not followed", target));
                        break;
                    }
                    Err(_) => {
                        report.problems.push(format!(
                            "{}: cannot inspect scope; symlinks are not followed",
                            target
                        ));
                        break;
                    }
                }
            }
        }
        if targets.len() > 64 {
            report
                .problems
                .push("At most 64 instruction targets per request".into());
        }
        report.problems.sort();
        report.problems.dedup();
        report
    }
}
