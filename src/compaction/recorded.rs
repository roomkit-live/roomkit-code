//! What the application recorded of a conversation, which a compaction's
//! summary carries beside what its model wrote: the plans the agent holds
//! there and the files the conversation changed. The model summarizes what
//! it is shown of the conversation; a plan or a changed file it leaves out
//! would be lost to the agent that continues, which would then plan anew or
//! not know what it touched.

use std::sync::Arc;

use roomkit_core::rules::transcript::json_text_line;
use roomkit_tools_plans::{Plan, StepStatus};

use crate::files::{Change, Effect, Files};
use crate::plans::Plans;

/// The most plans a summary names.
const PLANS: usize = 5;
/// The most steps under way a plan's line names.
const STEPS: usize = 5;
/// The most changed files a summary names: the latest changed, those the
/// file tools changed before those found only around shell commands.
const FILES: usize = 40;
/// How a summary introduces the files the file tools changed.
const BY_TOOLS: &str = "Files changed in this conversation with the file tools";
/// How it introduces those found only by comparing the project around shell
/// commands: what a command changed, and whatever else changed meanwhile.
const AROUND_COMMANDS: &str = "Files that changed only while this conversation's shell commands ran, by a command such as a merge or by another program meanwhile";

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
        lines.extend(files_lines(&self.files.changes(room).await));
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

/// The files `changes` name, the latest changed first: a line for those the
/// file tools changed, then one for those found only around shell commands.
/// A merge run through the shell changes the files of the commits it brings:
/// in one list they came before the agent's own, which the bound then left
/// out. The two lines name `FILES` files in all, the first one first.
fn files_lines(changes: &[Effect]) -> Vec<String> {
    let (commands, tools): (Vec<&Effect>, Vec<&Effect>) =
        changes.iter().partition(|file| file.shell);
    let left = FILES.saturating_sub(tools.len());
    [(BY_TOOLS, tools, FILES), (AROUND_COMMANDS, commands, left)]
        .into_iter()
        .filter(|(_, files, _)| !files.is_empty())
        .map(|(what, files, room)| files_line(what, &files, room))
        .collect()
}

/// `what`, then at most `room` of `files` named, and how many more there are.
fn files_line(what: &str, files: &[&Effect], room: usize) -> String {
    let named: Vec<String> = files
        .iter()
        .take(room)
        .map(|file| {
            let how = match file.change {
                Change::Created => "created",
                Change::Modified => "modified",
                Change::Deleted => "deleted",
            };
            format!("{how} {}", json_text_line(&file.path))
        })
        .collect();
    let list = match (named.len(), files.len().saturating_sub(named.len())) {
        (0, more) => format!("{more}, not named here"),
        (_, 0) => named.join(", "),
        (_, more) => format!("{}, and {more} more", named.join(", ")),
    };
    format!("- {what}: {list}.")
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

    fn file(path: &str, change: Change, shell: bool) -> Effect {
        Effect {
            path: path.into(),
            change,
            shell,
        }
    }

    #[test]
    fn the_files_changed_are_named_up_to_a_bound() {
        assert!(files_lines(&[]).is_empty());
        let few = [
            file("src/new file.rs", Change::Created, false),
            file("old.rs", Change::Deleted, false),
        ];
        assert_eq!(
            files_lines(&few),
            [format!(
                "- {BY_TOOLS}: created \"src/new file.rs\", deleted \"old.rs\"."
            )]
        );
        let many: Vec<_> = (0..FILES + 3)
            .map(|n| file(&format!("f{n}.rs"), Change::Modified, false))
            .collect();
        let lines = files_lines(&many);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].ends_with("modified \"f39.rs\", and 3 more."),
            "{}",
            lines[0]
        );
    }

    /// Found in use: a merge run through the shell listed the files of the
    /// commits it brought before the agent's own, and the bound left 14 of
    /// the agent's out.
    #[test]
    fn the_files_of_the_file_tools_come_before_those_only_commands_changed() {
        // The latest changes are the merge's; the agent's own edit is older.
        let mut changes: Vec<_> = (0..FILES)
            .map(|n| file(&format!("merged{n}.rs"), Change::Modified, true))
            .collect();
        changes.push(file("src/own.rs", Change::Modified, false));
        let lines = files_lines(&changes);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0], format!("- {BY_TOOLS}: modified \"src/own.rs\"."));
        assert!(
            lines[1].starts_with(&format!("- {AROUND_COMMANDS}: modified \"merged0.rs\", ")),
            "{}",
            lines[1]
        );
        // One of the bound's 40 names went to the agent's own file.
        assert!(
            lines[1].ends_with("modified \"merged38.rs\", and 1 more."),
            "{}",
            lines[1]
        );
        // The file tools' files can take the whole bound: the others are counted.
        let full: Vec<_> = (0..FILES)
            .map(|n| file(&format!("own{n}.rs"), Change::Modified, false))
            .chain([file("merged.rs", Change::Created, true)])
            .collect();
        assert_eq!(
            files_lines(&full)[1],
            format!("- {AROUND_COMMANDS}: 1, not named here.")
        );
        // Without a file of the file tools, the commands' line stands alone.
        let only = [file("Cargo.lock", Change::Modified, true)];
        assert_eq!(
            files_lines(&only),
            [format!("- {AROUND_COMMANDS}: modified \"Cargo.lock\".")]
        );
    }
}
