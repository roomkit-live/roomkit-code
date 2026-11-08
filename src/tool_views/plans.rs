use super::*;
use roomkit_tools_plans::{Plan, PlanDeleteResult, PlanListResult, StepStatus};

pub(super) struct Plans;
impl ToolRenderer for Plans {
    fn activity(&self, call: &ToolCall) -> Option<String> {
        super::labels::activity(call)
    }

    fn render(&self, call: &ToolCall) -> Vec<String> {
        self.render_styled(call, false)
            .into_iter()
            .map(|line| line.into_iter().map(|s| s.text).collect())
            .collect()
    }
    fn has_expanded_details(&self, call: &ToolCall) -> bool {
        output::<Plan>(call).is_some_and(|p| p.steps.len() > 2 || !p.objective.is_empty())
            || output::<PlanListResult>(call).is_some_and(|p| p.plans.len() > 2)
    }
    fn render_styled(&self, call: &ToolCall, expanded: bool) -> Vec<Line> {
        if let Some(plan) = output::<Plan>(call) {
            let mut lines = vec![vec![
                Span::new(plan.title.clone(), Tone::Heading),
                Span::new(format!(" · {}", crate::plans::progress(&plan)), Tone::Muted),
            ]];
            if expanded {
                lines.extend(content::plain(&plan.objective));
            }
            let steps = if expanded { plan.steps.len() } else { 2 };
            for step in plan.steps.iter().take(steps) {
                lines.push(vec![Span::new(
                    crate::plans::step_label(step),
                    match step.status {
                        StepStatus::Completed => Tone::Success,
                        StepStatus::InProgress => Tone::Accent,
                        StepStatus::Blocked => Tone::Warning,
                        _ => Tone::Muted,
                    },
                )]);
            }
            if plan.steps.len() > steps {
                lines.push(vec![Span::new(
                    format!("{} more steps · /plans", plan.steps.len() - steps),
                    Tone::Muted,
                )]);
            }
            return lines;
        }
        if let Some(list) = output::<PlanListResult>(call) {
            let mut lines = vec![vec![Span::new(
                format!("{} plans", list.plans.len()),
                Tone::Muted,
            )]];
            for plan in list.plans.iter().take(if expanded { 64 } else { 2 }) {
                lines.push(vec![Span::new(
                    format!(
                        "{} · {}/{} completed",
                        plan.title, plan.completed_steps, plan.total_steps
                    ),
                    Tone::Text,
                )]);
            }
            return lines;
        }
        if let Some(deleted) = output::<PlanDeleteResult>(call) {
            return vec![vec![Span::new(
                if deleted.deleted {
                    "Plan deleted"
                } else {
                    "Plan was not deleted"
                },
                Tone::Muted,
            )]];
        }
        content::json_or_plain(&call.text)
    }
}
