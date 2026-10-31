use super::Plans;
use roomkit_cli_roomkit::{CommandResult, SessionConfig};
use roomkit_tools_plans::{Plan, PlanScope, PlanStep, StepStatus};
use roomkit_tui::protocol::{
    Choice, Event, PanelSpec, SidebarAction, SidebarContent, SidebarEntry, SidebarSection,
    SidebarTone,
};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
struct Panel {
    id: String,
    scope: PlanScope,
    plan: Option<String>,
}

pub(crate) struct PlanView {
    store: Plans,
    panel: Mutex<Option<Panel>>,
}

fn scope(view: &SessionConfig) -> PlanScope {
    PlanScope {
        room_id: view.room.to_string(),
        caller_id: view.agent.to_string(),
    }
}
fn action(arguments: impl Into<String>) -> Option<SidebarAction> {
    Some(SidebarAction {
        command: "plans".into(),
        arguments: arguments.into(),
    })
}

fn step_preview(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.len() <= 1024 {
        return text;
    }
    let end = text.floor_char_boundary(1021);
    format!("{}…", &text[..end])
}

pub(crate) fn progress(plan: &Plan) -> String {
    let done = plan
        .steps
        .iter()
        .filter(|s| s.status == StepStatus::Completed)
        .count();
    let cancelled = plan
        .steps
        .iter()
        .filter(|s| s.status == StepStatus::Cancelled)
        .count();
    let blocked = plan
        .steps
        .iter()
        .filter(|s| s.status == StepStatus::Blocked)
        .count();
    let total = plan.steps.len();
    let mut text = if total == 0 {
        "No steps yet".into()
    } else {
        format!("{done}/{total} completed")
    };
    if blocked > 0 {
        text.push_str(&format!(" · {blocked} blocked"));
    }
    if cancelled > 0 {
        text.push_str(&format!(" · {cancelled} cancelled"));
    }
    text
}

pub(crate) fn step_label(step: &PlanStep) -> String {
    let marker = match step.status {
        StepStatus::Pending => "○",
        StepStatus::InProgress => "▶",
        StepStatus::Blocked => "!",
        StepStatus::Completed => "✓",
        StepStatus::Cancelled => "−",
    };
    format!("{marker} {}", step.description)
}

impl PlanView {
    pub fn new(store: Plans) -> Arc<Self> {
        Arc::new(Self {
            store,
            panel: Mutex::new(None),
        })
    }

    pub async fn handle(&self, view: &SessionConfig, args: &str) -> Result<CommandResult, String> {
        let scope = scope(view);
        let mut panel = self.panel.lock().await;
        let plan = if let Some((identity, verb)) = args.split_once(':') {
            let current = panel
                .as_ref()
                .filter(|p| p.scope == scope && p.id == identity)
                .ok_or("This plan view has expired; reopen /plans")?;
            if verb == "dismiss" {
                *panel = None;
                return Ok(CommandResult::Events(Vec::new()));
            }
            if verb == "list" {
                None
            } else if let Some(id) = verb.strip_prefix("open:") {
                Some(id.to_owned())
            } else if verb == "follow" {
                let id = current.plan.as_ref().ok_or("Choose a plan first")?.clone();
                self.store
                    .select(&scope, &id)
                    .await
                    .map_err(|e| e.to_string())?;
                Some(id)
            } else {
                return Err("Unknown plan action".into());
            }
        } else if args.is_empty() {
            None
        } else {
            Some(args.to_owned())
        };
        let document = self
            .store
            .snapshot(&scope)
            .await
            .map_err(|e| e.to_string())?;
        if plan
            .as_ref()
            .is_some_and(|id| !document.plans.contains_key(id))
        {
            return Err("Unknown plan for this conversation and agent".into());
        }
        let next = Panel {
            id: uuid::Uuid::new_v4().to_string(),
            scope,
            plan,
        };
        let spec = self.render(&next).await;
        *panel = Some(next);
        Ok(CommandResult::Events(vec![Event::Panel {
            conversation: view.room.as_str().into(),
            panel: spec,
        }]))
    }

    pub async fn updates(&self, view: &SessionConfig) -> Vec<Event> {
        let panel = self
            .panel
            .lock()
            .await
            .clone()
            .filter(|p| p.scope == scope(view));
        match panel {
            Some(panel) => vec![Event::PanelUpdated {
                conversation: view.room.as_str().into(),
                panel: self.render(&panel).await,
            }],
            None => Vec::new(),
        }
    }

    async fn render(&self, panel: &Panel) -> PanelSpec {
        let mut spec = PanelSpec {
            id: panel.id.clone(),
            command: "plans".into(),
            title: "Plans".into(),
            body: String::new(),
            actions: Vec::new(),
            dismiss: format!("{}:dismiss", panel.id),
        };
        let choose = |value: String, label: String, description: String| Choice {
            value: format!("{}:{value}", panel.id),
            label,
            description,
        };
        let doc = match self.store.snapshot(&panel.scope).await {
            Ok(doc) => doc,
            Err(e) => {
                spec.body = e.to_string();
                return spec;
            }
        };
        if let Some(id) = &panel.plan {
            spec.actions
                .push(choose("list".into(), "All plans".into(), String::new()));
            let Some(plan) = doc.plans.get(id) else {
                spec.body = "This plan was deleted.".into();
                return spec;
            };
            spec.title = plan.title.clone();
            spec.body = format!("{}\n\n{}\n\n", progress(plan), plan.objective);
            for step in &plan.steps {
                spec.body.push_str(&format!("{}\n", step_label(step)));
            }
            spec.body.push_str(
                "\nProgress is reported by the agent. Changing a plan never executes its steps.",
            );
            if doc.selected.as_ref() != Some(id) {
                spec.actions.push(choose(
                    "follow".into(),
                    "Follow in sidebar".into(),
                    "Keep this plan visible while you work".into(),
                ));
            }
        } else {
            spec.body = if doc.plans.is_empty() {
                "No plan yet. Switch to Plan with Shift+Tab and ask the agent to prepare one."
                    .into()
            } else {
                "Choose a plan to inspect its objective and steps. Plans are specific to this conversation and agent.".into()
            };
            for plan in doc.plans.values() {
                spec.actions.push(choose(
                    format!("open:{}", plan.plan_id),
                    plan.title.clone(),
                    format!(
                        "{}{}",
                        progress(plan),
                        if doc.selected.as_ref() == Some(&plan.plan_id) {
                            " · following"
                        } else {
                            ""
                        }
                    ),
                ));
            }
        }
        spec
    }

    pub async fn sidebar(&self, view: &SessionConfig) -> SidebarSection {
        let mut section = SidebarSection {
            title: "Plan".into(),
            action: action(""),
            content: SidebarContent::Entries(Vec::new()),
        };
        let mut entries = Vec::new();
        match self.store.snapshot(&scope(view)).await {
            Ok(doc) => {
                if let Some(plan) = doc.selected.as_ref().and_then(|id| doc.plans.get(id)) {
                    section.action = action(&plan.plan_id);
                    entries.push(SidebarEntry {
                        label: plan.title.clone(),
                        detail: progress(plan),
                        tone: if !plan.steps.is_empty()
                            && plan.steps.iter().all(|s| s.status == StepStatus::Completed)
                        {
                            SidebarTone::Success
                        } else {
                            SidebarTone::Neutral
                        },
                        action: action(&plan.plan_id),
                    });
                    // Keep active/blocked work visible; otherwise show the next pending step.
                    let mut focused = plan
                        .steps
                        .iter()
                        .enumerate()
                        .filter(|(_, step)| {
                            matches!(step.status, StepStatus::InProgress | StepStatus::Blocked)
                        })
                        .collect::<Vec<_>>();
                    if focused.is_empty() {
                        focused.extend(
                            plan.steps
                                .iter()
                                .enumerate()
                                .find(|(_, s)| s.status == StepStatus::Pending),
                        );
                    }
                    let count = focused.len();
                    for (index, step) in focused.into_iter().take(3) {
                        entries.push(SidebarEntry {
                            label: format!(
                                "Step {} · {}",
                                index + 1,
                                match step.status {
                                    StepStatus::InProgress => "In progress",
                                    StepStatus::Blocked => "Blocked",
                                    _ => "Next",
                                }
                            ),
                            detail: step_preview(&step.description),
                            tone: match step.status {
                                StepStatus::Blocked => SidebarTone::Warning,
                                StepStatus::InProgress => SidebarTone::Accent,
                                _ => SidebarTone::Muted,
                            },
                            action: action(&plan.plan_id),
                        });
                    }
                    if count > 3 {
                        entries.push(SidebarEntry {
                            label: format!("{} more active steps", count - 3),
                            detail: String::new(),
                            tone: SidebarTone::Muted,
                            action: action(&plan.plan_id),
                        });
                    }
                    if doc.plans.len() > 1 {
                        entries.push(SidebarEntry {
                            label: format!("{} plans · choose", doc.plans.len()),
                            detail: String::new(),
                            tone: SidebarTone::Muted,
                            action: action(""),
                        });
                    }
                } else {
                    entries.push(SidebarEntry {
                        label: if doc.plans.is_empty() {
                            "No plan yet".into()
                        } else {
                            format!("{} plans · choose one to follow", doc.plans.len())
                        },
                        detail: String::new(),
                        tone: SidebarTone::Muted,
                        action: action(""),
                    });
                }
            }
            Err(_) => entries.push(SidebarEntry {
                label: "Plan storage unavailable".into(),
                detail: "Open /plans for details".into(),
                tone: SidebarTone::Warning,
                action: action(""),
            }),
        }
        section.content = SidebarContent::Entries(entries);
        section
    }
}
