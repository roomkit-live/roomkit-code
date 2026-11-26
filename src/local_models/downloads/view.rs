//! The same transfer measurements in the sidebar and the narrow-terminal panel.
use super::{Active, Downloads, percentage};
use roomkit_tui::protocol::{
    PanelSpec, SidebarAction, SidebarContent, SidebarEntry, SidebarSection, SidebarTone,
};

impl Downloads {
    fn entries(&self) -> Vec<SidebarEntry> {
        let state = crate::sync::lock(&self.state);
        let mut rows: Vec<_> = state.active.values().take(16).map(active_entry).collect();
        if state.active.len() > rows.len() {
            rows.push(SidebarEntry::new(format!(
                "{} more active downloads",
                state.active.len() - rows.len()
            )));
        }
        for (_, name, outcome) in &state.recent {
            let (detail, tone) = match outcome {
                Ok(()) => ("Downloaded and ready to select", SidebarTone::Success),
                Err(error) => (error.as_str(), SidebarTone::Error),
            };
            rows.push(SidebarEntry::new(name).with_detail(detail).with_tone(tone));
        }
        rows
    }

    pub fn sidebar(&self) -> Option<SidebarSection> {
        let entries = self.entries();
        (!entries.is_empty()).then(|| {
            SidebarSection::new("Model downloads", SidebarContent::Entries { entries })
                .with_action(SidebarAction::new("downloads", ""))
        })
    }

    pub fn panel(&self) -> PanelSpec {
        let rows: Vec<_> = self
            .entries()
            .into_iter()
            .map(|entry| format!("{}\n{}", entry.label, entry.detail))
            .collect();
        let body = if rows.is_empty() {
            "No model downloads.".into()
        } else {
            rows.join("\n\n")
        };
        PanelSpec::new(
            "local-model-downloads",
            "downloads",
            "Model downloads",
            body,
        )
        .with_dismiss("close")
    }
}

fn active_entry(job: &Active) -> SidebarEntry {
    let detail = job.bytes.map_or_else(
        || "Preparing download".into(),
        |(done, total)| {
            let percent = percentage(done, total);
            let filled = (percent / 10) as usize;
            let stage = if done == total { " · verifying" } else { "" };
            format!(
                "{}{} {percent}% · {} / {}{stage}",
                "█".repeat(filled),
                "░".repeat(10 - filled),
                size(done),
                size(total)
            )
        },
    );
    SidebarEntry::new(&job.name).with_detail(detail)
}

fn size(bytes: u64) -> String {
    match bytes {
        0..1000 => format!("{bytes} B"),
        1000..1_000_000 => format!("{:.1} kB", bytes as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1} MB", bytes as f64 / 1e6),
        _ => format!("{:.2} GB", bytes as f64 / 1e9),
    }
}
