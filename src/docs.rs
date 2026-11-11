//! RoomKit Code's own user guide, served read-only through the documentation
//! tools. The guide is this crate's USER_GUIDE.md, embedded at build time and split
//! into one document per level-two section: the tools return one excerpt per
//! document, so a single large file would make search land on unrelated lines.

use roomkit_tools_docs::{DocsBackend, FileDocs, FileDocsLimits};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::Result;

/// The user guide is part of the package, so every installed binary carries it.
const GUIDE: &str = include_str!("../USER_GUIDE.md");

/// One document of the guide, written as `file` in the guide directory.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Topic {
    pub file: String,
    pub text: String,
}

/// Serve the guide from `<state>/docs/<revision>`, written once per content
/// revision. Other revisions stay in place: another running version of the
/// application may still load its own guide on first use.
pub(crate) fn guide(state: &Path) -> Result<Arc<dyn DocsBackend>> {
    let root = materialize(&state.join("docs"), &topics(GUIDE))?;
    let limits = FileDocsLimits {
        // A whole section fits one page without flooding the terminal or context.
        max_read_bytes: 16 * 1024,
        ..FileDocsLimits::default()
    };
    Ok(Arc::new(
        FileDocs::new(root, limits)?.with_version(env!("CARGO_PKG_VERSION"))?,
    ))
}

/// Split Markdown at its `## ` headings outside fenced code. Text before the
/// first heading becomes `overview.md`. Each section is retitled as a level-one
/// heading, which the documentation tools use as the document title.
pub(crate) fn topics(guide: &str) -> Vec<Topic> {
    let mut sections: Vec<(&str, Vec<String>)> = vec![("Overview", Vec::new())];
    let mut fence = None;
    for line in guide.lines() {
        let heading = line.strip_prefix("## ").filter(|_| fence.is_none());
        fence = next_fence(fence, line);
        if let Some(title) = heading {
            sections.push((title.trim(), vec![format!("# {}", title.trim())]));
        } else if let Some((_, lines)) = sections.last_mut() {
            lines.push(line.into());
        }
    }
    let mut files = BTreeSet::new();
    sections
        .into_iter()
        .filter(|(_, lines)| lines.iter().any(|line| !line.trim().is_empty()))
        .map(|(title, lines)| Topic {
            file: unique(&mut files, &slug(title)),
            text: format!("{}\n", lines.join("\n").trim_end()),
        })
        .collect()
}

/// Track fenced code so a `## ` line inside an example is not a heading.
fn next_fence(open: Option<char>, line: &str) -> Option<char> {
    let marker = line.trim_start();
    let fence = ['`', '~']
        .into_iter()
        .find(|c| marker.starts_with(&c.to_string().repeat(3)));
    match (open, fence) {
        (None, Some(found)) => Some(found),
        (Some(open), Some(found)) if open == found => None,
        (open, _) => open,
    }
}

fn slug(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() { "section" } else { slug }.into()
}

fn unique(files: &mut BTreeSet<String>, slug: &str) -> String {
    let mut file = format!("{slug}.md");
    let mut index = 2;
    while !files.insert(file.clone()) {
        file = format!("{slug}-{index}.md");
        index += 1;
    }
    file
}

/// Publish the topics atomically: a complete directory appears under its
/// content revision, or none does. Concurrent starts converge on one copy.
pub(crate) fn materialize(parent: &Path, topics: &[Topic]) -> std::io::Result<PathBuf> {
    let mut hash = Sha256::new();
    for topic in topics {
        hash.update(topic.file.as_bytes());
        hash.update([0]);
        hash.update(topic.text.as_bytes());
        hash.update([0]);
    }
    let revision = crate::digest::hex(&hash.finalize());
    let target = parent.join(&revision[..16]);
    if target.is_dir() {
        return Ok(target);
    }
    std::fs::create_dir_all(parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(parent)?;
    for topic in topics {
        std::fs::write(staging.path().join(&topic.file), &topic.text)?;
    }
    match std::fs::rename(staging.path(), &target) {
        Ok(()) => {
            // The directory moved to `target`; skip the staging cleanup.
            let _ = staging.keep();
            Ok(target)
        }
        // Another instance published the same revision first.
        Err(_) if target.is_dir() => Ok(target),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
