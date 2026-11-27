//! The same transfer measurements in the sidebar and the narrow-terminal panel.
use super::{Active, Downloads, State, percentage};
use roomkit_tui::protocol::{
    BackgroundStatus, Choice, Event, PanelSpec, SidebarAction, SidebarContent, SidebarEntry,
    SidebarSection, SidebarTone,
};

impl Downloads {
    fn entries(state: &State) -> Vec<SidebarEntry> {
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

    /// Expose progress beside the composer when its background shortcut is free.
    pub fn show_progress(&self, events: &mut [Event]) {
        let state = crate::sync::lock(&self.state);
        let Some(job) = state.active.values().next() else {
            return;
        };
        let label = job.bytes.map_or_else(
            || "GGUF: preparing".to_owned(),
            |(done, total)| {
                let pct = percentage(done, total);
                format!("GGUF {} {pct}%", bar(pct))
            },
        );
        for event in events {
            if let Event::BackgroundChanged { status, .. } = event
                && status.is_none()
            {
                *status = Some(BackgroundStatus::new(&label, "downloads", ""));
            }
        }
    }

    pub fn sidebar(&self) -> Option<SidebarSection> {
        let entries = Self::entries(&crate::sync::lock(&self.state));
        (!entries.is_empty()).then(|| {
            SidebarSection::new("Model downloads", SidebarContent::Entries { entries })
                .with_action(SidebarAction::new("downloads", ""))
        })
    }

    pub fn panel(&self) -> PanelSpec {
        let state = crate::sync::lock(&self.state);
        let rows: Vec<_> = Self::entries(&state)
            .into_iter()
            .map(|entry| format!("{}\n{}", entry.label, entry.detail))
            .collect();
        let body = if rows.is_empty() {
            "No model downloads.".into()
        } else if state.active.is_empty() {
            rows.join("\n\n")
        } else {
            format!(
                "Press Esc to keep chatting while downloads continue.\n\n{}",
                rows.join("\n\n")
            )
        };
        // Keep destructive actions off the first row as transfers finish or the view opens.
        let mut actions = vec![Choice::new("installed", "Manage installed models")];
        for job in state.active.values().take(16) {
            actions.push(Choice::new(
                format!("cancel {}", job.ticket),
                format!("Cancel {}", short_name(&job.name)),
            ));
        }
        for (id, name, outcome) in &state.recent {
            if outcome.is_ok() {
                actions.push(Choice::new(
                    format!("use {id}"),
                    format!("Use {}", short_name(name)),
                ));
            }
        }
        PanelSpec::new(
            "local-model-downloads",
            "downloads",
            "Model downloads",
            body,
        )
        .with_actions(actions)
        .with_dismiss("close")
    }
}

fn short_name(name: &str) -> String {
    name.chars().take(60).collect()
}

fn active_entry(job: &Active) -> SidebarEntry {
    let detail = job.bytes.map_or_else(
        || "Preparing download".into(),
        |(done, total)| {
            let percent = percentage(done, total);
            let stage = if done == total { " · verifying" } else { "" };
            format!(
                "{} {percent}% · {} / {}{stage}",
                bar(percent),
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

fn bar(percent: u64) -> String {
    let filled = (percent / 10) as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
}
