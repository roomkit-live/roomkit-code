use super::*;
use roomkit_tui::protocol::{
    Choice, Event, PanelSpec, SidebarAction, SidebarContent, SidebarEntry, SidebarSection,
    SidebarTone,
};
use roomkit_tui_engine::CommandResult;

#[derive(Clone)]
pub(super) struct Panel {
    id: String,
    room: String,
    agent: String,
    hook: Option<String>,
    offset: usize,
}

fn action(value: String, label: impl Into<String>, description: impl Into<String>) -> Choice {
    Choice::new(value, label).with_description(description)
}

impl Hooks {
    pub fn panel(&self, view: &SessionConfig, args: &str) -> Result<CommandResult, String> {
        let mut state = self.state();
        let mut selected = None;
        let mut offset = 0;
        if !args.is_empty() && args != "list" {
            let (identity, verb) = args
                .split_once(':')
                .ok_or("Use /hooks to inspect and manage hooks")?;
            let panel = state
                .panel
                .as_ref()
                .filter(|p| {
                    p.id == identity
                        && p.room == view.room.as_str()
                        && p.agent == view.agent.as_str()
                })
                .cloned()
                .ok_or("This hooks view expired; reopen /hooks")?;
            offset = panel.offset;
            match verb {
                "dismiss" => {
                    state.panel = None;
                    return Ok(CommandResult::Events(vec![]));
                }
                "reload" | "enable" | "disable" if !state.turns.is_empty() => {
                    return Err("Wait for the current turn to finish before changing hooks".into());
                }
                "reload" => {
                    state.catalogue.reload();
                    offset = 0;
                }
                "list" => {}
                "next" => {
                    offset = offset
                        .saturating_add(60)
                        .min(state.catalogue.entries.len().saturating_sub(1))
                }
                "previous" => offset = offset.saturating_sub(60),
                "enable" | "disable" => {
                    let id = panel.hook.ok_or("Choose a hook first")?;
                    state.catalogue.set_enabled(&id, verb == "enable")?;
                    selected = Some(id);
                }
                _ if verb.starts_with("open:") => selected = Some(verb[5..].into()),
                _ => return Err("Unknown hook action".into()),
            }
        }
        let panel = Panel {
            id: uuid::Uuid::new_v4().to_string(),
            room: view.room.to_string(),
            agent: view.agent.to_string(),
            hook: selected,
            offset,
        };
        let spec = specification(&state, &panel)?;
        state.panel = Some(panel);
        drop(state);
        self.changes.send_replace(());
        Ok(CommandResult::Events(vec![Event::Panel {
            conversation: view.room.as_str().into(),
            panel: spec,
        }]))
    }

    pub fn updates(&self, view: &SessionConfig) -> Vec<Event> {
        let state = self.state();
        state
            .panel
            .as_ref()
            .filter(|p| p.room == view.room.as_str() && p.agent == view.agent.as_str())
            .and_then(|p| specification(&state, p).ok())
            .map(|panel| {
                vec![Event::PanelUpdated {
                    conversation: view.room.as_str().into(),
                    panel,
                }]
            })
            .unwrap_or_default()
    }

    pub fn sidebar(&self, view: &SessionConfig) -> Option<SidebarSection> {
        let state = self.state();
        if state.catalogue.entries.is_empty() && state.catalogue.problems.is_empty() {
            return None;
        }
        let active = state
            .recent
            .iter()
            .filter(|r| {
                r.room == view.room.as_str()
                    && r.agent == view.agent.as_str()
                    && r.status == "running"
            })
            .count();
        let recent = state
            .recent
            .iter()
            .rev()
            .find(|r| r.room == view.room.as_str() && r.agent == view.agent.as_str());
        let detail = if !state.catalogue.problems.is_empty() {
            "Configuration needs attention · open /hooks".into()
        } else if active > 0 {
            format!(
                "{active} running · {}",
                recent.map_or("", |r| r.label.as_str())
            )
        } else if let Some(receipt) = recent {
            format!(
                "{} · {} ms · {}",
                receipt.status, receipt.elapsed_ms, receipt.label
            )
        } else {
            format!(
                "{} enabled · {} need review or disabled",
                state.catalogue.selected().len(),
                state
                    .catalogue
                    .entries
                    .iter()
                    .filter(|e| !state.catalogue.enabled.contains(&e.id))
                    .count()
            )
        };
        Some(
            SidebarSection::new(
                "Hooks",
                SidebarContent::Entries {
                    entries: vec![
                        SidebarEntry::new(if active > 0 {
                            "Running automation"
                        } else {
                            "Command hooks"
                        })
                        .with_detail(bounded(&detail, 512))
                        .with_tone(
                            if !state.catalogue.problems.is_empty()
                                || recent
                                    .is_some_and(|r| r.status == "failed" || r.status == "blocked")
                            {
                                SidebarTone::Warning
                            } else {
                                SidebarTone::Muted
                            },
                        )
                        .with_action(SidebarAction::new("hooks", "")),
                    ],
                },
            )
            .with_action(SidebarAction::new("hooks", "")),
        )
    }
}

fn specification(state: &State, panel: &Panel) -> Result<PanelSpec, String> {
    let prefix = &panel.id;
    let (title, body, actions) = if let Some(id) = &panel.hook {
        let entry = state
            .catalogue
            .entries
            .iter()
            .find(|e| &e.id == id)
            .ok_or("Hook no longer exists; reopen /hooks")?;
        let enabled = state.catalogue.enabled.contains(id);
        let receipts = state
            .recent
            .iter()
            .rev()
            .filter(|r| r.hook == *id && r.room == panel.room && r.agent == panel.agent)
            .take(3)
            .map(|r| {
                format!(
                    "{} · {} · {} ms\nTurn: {}\n{}",
                    r.event.name(),
                    r.status,
                    r.elapsed_ms,
                    r.turn.as_deref().unwrap_or("session"),
                    bounded(&r.detail, 6000)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        (
            format!("Hook · {}", entry.label()),
            format!(
                "State: {}\nSource: {}\nEvent: {}\nMatcher: {}\nTimeout: {} s\nRuns in Plan: {}\nWorking directory: project root\n\nCommand:\n{}\n\nEnabling trusts this exact definition for this project. Commands run with your user permissions and may read or modify data. Referenced scripts remain your responsibility. Changes to the definition require review after reload.\n\nRecent executions in this session (output is not saved to diagnostics):\n{}",
                if enabled {
                    "enabled"
                } else {
                    "disabled / needs review"
                },
                entry.source.display(),
                entry.event.name(),
                if entry.group.matcher.is_empty() {
                    "*"
                } else {
                    &entry.group.matcher
                },
                entry.hook.timeout,
                entry.hook.roomkit.run_in_plan,
                bounded(&entry.hook.command, 8192),
                if receipts.is_empty() {
                    "None"
                } else {
                    &receipts
                }
            ),
            vec![
                action(
                    format!("{prefix}:{}", if enabled { "disable" } else { "enable" }),
                    if enabled {
                        "Disable"
                    } else {
                        "Enable this hook"
                    },
                    "Apply only to the reviewed definition in this project",
                ),
                action(
                    format!("{prefix}:list"),
                    "All hooks",
                    "Return to the inventory",
                ),
            ],
        )
    } else {
        let mut actions = vec![action(
            format!("{prefix}:reload"),
            "Reload configuration",
            "Load global and project definitions; new or changed hooks need review",
        )];
        if panel.offset > 0 {
            actions.push(action(
                format!("{prefix}:previous"),
                "Previous page",
                "Earlier hooks",
            ));
        }
        if panel.offset + 60 < state.catalogue.entries.len() {
            actions.push(action(format!("{prefix}:next"), "Next page", "More hooks"));
        }
        actions.extend(
            state
                .catalogue
                .entries
                .iter()
                .skip(panel.offset)
                .take(60)
                .map(|e| {
                    action(
                        format!("{prefix}:open:{}", e.id),
                        e.label(),
                        bounded(
                            &format!(
                                "{} · {} · {}",
                                e.event.name(),
                                if state.catalogue.enabled.contains(&e.id) {
                                    "enabled"
                                } else {
                                    "needs review / disabled"
                                },
                                e.source.display()
                            ),
                            512,
                        ),
                    )
                }),
        );
        (
            "Hooks · configuration and activity".into(),
            format!(
                "Global: {}\nProject: {}\n\n{} hooks discovered; {} enabled for this project.\nChoose a hook to inspect its command, enable/disable it and view recent results.\nDisabled by default; no .claude or .codex configuration is executed automatically.\nPlan skips commands unless their roomkit.run_in_plan option is explicitly true.\n\nSupported: SessionStart, SessionEnd, UserPromptSubmit, TurnStart, TurnEnd, PreToolUse, PostToolUse, PostToolUseFailure.\nStop continuation and input rewriting are not implemented; unsupported fields are rejected.\n\n{}",
                state.catalogue.paths[0].display(),
                state.catalogue.paths[1].display(),
                state.catalogue.entries.len(),
                state.catalogue.selected().len(),
                state.catalogue.problems.join("\n")
            ),
            actions,
        )
    };
    Ok(PanelSpec::new(panel.id.clone(), "hooks", title, body)
        .with_actions(actions)
        .with_dismiss(format!("{prefix}:dismiss")))
}
