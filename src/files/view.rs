//! The sidebar section and the `/files` panel.

use roomkit_tui::protocol::{
    Event, PanelSpec, SidebarAction, SidebarContent, SidebarEntry, SidebarSection, SidebarTone,
};
use roomkit_tui_engine::{CommandResult, SessionConfig};

use super::Files;
use super::record::{Change, Changed, RETAINED, Record};

/// Most recent changes listed in the sidebar; `/files` lists them all.
pub(super) const SIDEBAR_ENTRIES: usize = 8;

impl Files {
    pub async fn sidebar(&self, room: &str) -> SidebarSection {
        let record = self.record(room).await;
        let open = || SidebarAction::new("files", "");
        let changes = record.changes();
        let mut entries: Vec<_> = changes
            .iter()
            .take(SIDEBAR_ENTRIES)
            .map(|c| {
                SidebarEntry::new(c.path.as_str())
                    .with_detail(status(c))
                    .with_tone(tone(c.change))
                    .with_action(open())
            })
            .collect();
        if changes.len() > SIDEBAR_ENTRIES {
            let more = changes.len() - SIDEBAR_ENTRIES;
            entries.push(
                SidebarEntry::new(format!("{more} more"))
                    .with_tone(SidebarTone::Muted)
                    .with_action(open()),
            );
        }
        if changes.is_empty() {
            entries.push(SidebarEntry::new("No files changed").with_tone(SidebarTone::Muted));
        }
        let reads = record.reads().len();
        if reads > 0 {
            entries.push(
                SidebarEntry::new(format!("{reads} {} read", plural(reads, "file")))
                    .with_tone(SidebarTone::Muted)
                    .with_action(open()),
            );
        }
        if record.untracked {
            entries.push(
                SidebarEntry::new("Shell changes not tracked")
                    .with_detail("Project too large to compare")
                    .with_tone(SidebarTone::Warning),
            );
        }
        SidebarSection::new("Files", SidebarContent::Entries { entries }).with_action(open())
    }

    pub async fn panel(
        &self,
        view: &SessionConfig,
        arguments: &str,
    ) -> Result<CommandResult, String> {
        match arguments {
            "" => {}
            "dismiss" => return Ok(CommandResult::Events(Vec::new())),
            _ => return Err("Use /files without arguments".into()),
        }
        let record = self.record(view.room.as_str()).await;
        Ok(CommandResult::Events(vec![Event::Panel {
            conversation: view.room.as_str().into(),
            panel: PanelSpec::new(
                uuid::Uuid::new_v4().to_string(),
                "files",
                "Files · this conversation",
                body(&record),
            )
            .with_dismiss("dismiss"),
        }]))
    }
}

pub(super) fn body(record: &Record) -> String {
    let mut body = String::from(
        "Files this conversation's agents changed, newest first. File tools report their own changes. A shell command's are found by comparing the project before and after it: anything that changed meanwhile is credited to it, including a background process, an editor or another instance. Between commands, background processes are not followed.\n",
    );
    let notes = [
        (
            record.untracked,
            "Some shell commands ran in a project too large to compare.".to_owned(),
        ),
        (
            record.trimmed,
            format!("Only the latest {RETAINED} changed and {RETAINED} read files are kept."),
        ),
        (
            record.offline,
            "The saved list could not be read; it shows changes since then and is not saved."
                .to_owned(),
        ),
        (
            record.unsaved,
            "This list could not be saved and may not survive a restart.".to_owned(),
        ),
    ];
    for (_, note) in notes.iter().filter(|(shown, _)| *shown) {
        body.push_str(note);
        body.push('\n');
    }
    let changes = record.changes();
    body.push_str(&format!("\nChanged ({})\n", changes.len()));
    if changes.is_empty() {
        body.push_str("  None yet\n");
    }
    for change in changes {
        body.push_str(&format!("  {:<16} {}\n", status(change), change.path));
    }
    let reads = record.reads();
    body.push_str(&format!("\nRead only ({})\n", reads.len()));
    for path in reads {
        body.push_str(&format!("  {path}\n"));
    }
    body
}

pub(super) fn status(change: &Changed) -> String {
    let label = match change.change {
        Change::Created => "Created",
        Change::Modified => "Modified",
        Change::Deleted => "Deleted",
    };
    if change.shell {
        format!("{label} · shell")
    } else {
        label.into()
    }
}

fn tone(change: Change) -> SidebarTone {
    match change {
        Change::Created => SidebarTone::Success,
        Change::Modified => SidebarTone::Accent,
        Change::Deleted => SidebarTone::Warning,
    }
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        word.into()
    } else {
        format!("{word}s")
    }
}
