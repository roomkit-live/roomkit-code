//! What the application recorded of a conversation, which a compaction's
//! summary carries beside what its model wrote: the plans the agent holds
//! there and the files the conversation changed. The model summarizes what
//! it is shown of the conversation; a plan or a changed file it leaves out
//! would be lost to the agent that continues, which would then plan anew or
//! not know what it touched.

use std::sync::Arc;

use roomkit_core::rules::transcript::json_text_line;
use roomkit_tools_plans::{Plan, StepStatus};

use crate::files::{Change, Files};
use crate::plans::Plans;

/// The most plans a summary names.
const PLANS: usize = 5;
/// The most steps under way a plan's line names.
const STEPS: usize = 5;
/// The most changed files a summary names: the latest changed.
const FILES: usize = 40;

/// What the application records of each conversation, for its summaries.
pub(crate) struct Recorded {
    plans: Arc<Plans>,
    files: Arc<Files>,
}

impl Recorded {
    pub fn new(plans: Arc<Plans>, files: Arc<Files>) -> Self {
        Self { plans, files }
    }

    /// What is recorded of `room` for `agent` as a summary is written: its
    /// plans, then the files the conversation changed. None when there is
    /// neither.
    pub(super) async fn of(&self, room: &str, agent: &str) -> Option<String> {
        let plans = self.plans.held(room, agent).await;
        let mut lines: Vec<String> = plans.iter().take(PLANS).map(plan_line).collect();
        if plans.len() > PLANS {
            lines.push(format!("- {} more plans.", plans.len() - PLANS));
        }
        lines.extend(files_line(&self.files.changes(room).await));
        (!lines.is_empty()).then(|| {
            format!(
                "Recorded by the application when this summary was written:\n{}",
                lines.join("\n")
            )
        })
    }
}

/// A plan on a line: its title, how far it is, the steps under way, and
/// what reads it again.
fn plan_line(plan: &Plan) -> String {
    let under_way: Vec<&str> = plan
        .steps
        .iter()
        .filter(|step| step.status == StepStatus::InProgress)
        .map(|step| step.step_id.as_str())
        .take(STEPS)
        .collect();
    let under_way = if under_way.is_empty() {
        String::new()
    } else {
        format!(", in progress: {}", under_way.join(", "))
    };
    format!(
        "- Plan {}: {}{under_way} (plan_id {}, revision {}).",
        json_text_line(&plan.title),
        crate::plans::progress(plan),
        plan.plan_id,
        plan.revision
    )
}

/// The files `changes` name, the latest changed first, on a line; none
/// without a change.
fn files_line(changes: &[(String, Change)]) -> Option<String> {
    if changes.is_empty() {
        return None;
    }
    let named: Vec<String> = changes
        .iter()
        .take(FILES)
        .map(|(path, change)| {
            let how = match change {
                Change::Created => "created",
                Change::Modified => "modified",
                Change::Deleted => "deleted",
            };
            format!("{how} {}", json_text_line(path))
        })
        .collect();
    let more = match changes.len().saturating_sub(FILES) {
        0 => String::new(),
        more => format!(", and {more} more"),
    };
    Some(format!(
        "- Files changed in this conversation: {}{more}.",
        named.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
    use super::*;
    use roomkit_tools_plans::PlanStep;

    fn step(id: &str, status: StepStatus) -> PlanStep {
        PlanStep {
            step_id: id.into(),
            description: "A step".into(),
            status,
        }
    }

    #[test]
    fn a_plan_s_line_says_how_far_it_is_and_what_reads_it_again() {
        let plan = Plan {
            plan_id: "plan-1".into(),
            revision: 4,
            title: "Fix the\ntools".into(),
            objective: "An objective".into(),
            steps: vec![
                step("first", StepStatus::Completed),
                step("second", StepStatus::InProgress),
                step("third", StepStatus::Blocked),
            ],
        };
        assert_eq!(
            plan_line(&plan),
            "- Plan \"Fix the\\ntools\": 1/3 completed · 1 blocked, in progress: second (plan_id plan-1, revision 4)."
        );
    }

    #[test]
    fn the_files_changed_are_named_up_to_a_bound() {
        assert_eq!(files_line(&[]), None);
        let few = [
            ("src/new file.rs".to_owned(), Change::Created),
            ("old.rs".to_owned(), Change::Deleted),
        ];
        assert_eq!(
            files_line(&few).unwrap(),
            "- Files changed in this conversation: created \"src/new file.rs\", deleted \"old.rs\"."
        );
        let many: Vec<_> = (0..FILES + 3)
            .map(|file| (format!("f{file}.rs"), Change::Modified))
            .collect();
        let line = files_line(&many).unwrap();
        assert!(line.ends_with("modified \"f39.rs\", and 3 more."), "{line}");
    }
}
