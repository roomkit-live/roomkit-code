//! The exit report of a conversation whose agents handed tasks over: what
//! the conversation consumed as a whole, then each agent's part.

use std::iter::once;

use roomkit_tui::protocol::ConversationCost;

use super::{Summary, Usage, amount};
use crate::delegation::{Task, spending};

/// One agent's part of a conversation's consumption.
#[derive(Debug)]
pub(crate) struct Spender {
    pub(super) label: String,
    /// How many delegated tasks it worked on; 0 for the conversation's own
    /// agent.
    pub(super) tasks: usize,
    pub(super) usage: Usage,
    pub(super) cost: Option<ConversationCost>,
}

impl Spender {
    /// `  Review (2 tasks): total=18000 · 2 turns · ≈ $0.88`.
    pub(super) fn line(&self) -> String {
        let tasks = match self.tasks {
            0 => String::new(),
            1 => " (1 task)".to_owned(),
            count => format!(" ({count} tasks)"),
        };
        let usage = &self.usage;
        let noun = if usage.turns == 1 { "turn" } else { "turns" };
        let cost = self
            .cost
            .as_ref()
            .map_or_else(String::new, |cost| format!(" · {}", amount(cost)));
        let whole = usage.complete && self.cost.as_ref().is_none_or(|cost| cost.complete);
        let partial = if whole { "" } else { " (partial)" };
        format!(
            "  {}{tasks}: total={} · {} {noun}{cost}{partial}",
            self.label,
            usage.input.saturating_add(usage.output),
            usage.turns,
        )
    }
}

impl Summary {
    /// Adds what the conversation's `tasks` consumed to its report: its usage
    /// and cost become the whole conversation's, and `lead`, its own agent,
    /// and each worker get a line. A task that still runs is cut by closing:
    /// the report is then partial.
    pub(crate) fn add_tasks(&mut self, lead: &str, tasks: &[Task]) {
        let cost = self.cost.clone();
        let Some(usage) = self.usage.as_mut() else {
            return;
        };
        usage.complete &= tasks.iter().all(|task| !task.running());
        let read: Vec<&Task> = tasks.iter().filter(|task| task.spent.is_some()).collect();
        if read.is_empty() {
            return;
        }
        let mut spenders = vec![Spender {
            label: lead.to_owned(),
            tasks: 0,
            usage: usage.clone(),
            cost: cost.clone(),
        }];
        let costs = read.iter().map(|task| spending::cost_of(task));
        self.cost = spending::sum(once(cost.as_ref()).chain(costs));
        for task in read {
            if let Some(spent) = &task.spent {
                usage.absorb(&spent.usage);
                add_to_worker(&mut spenders, task);
            }
        }
        self.spenders = spenders;
    }
}

/// Counts `task` in the line of its worker, which it opens if it is the
/// worker's first.
fn add_to_worker(spenders: &mut Vec<Spender>, task: &Task) {
    let Some(spent) = &task.spent else {
        return;
    };
    let line = spenders
        .iter_mut()
        .skip(1)
        .find(|spender| spender.label == task.agent);
    match line {
        Some(line) => {
            line.tasks += 1;
            line.usage.absorb(&spent.usage);
            line.cost = spending::sum([line.cost.as_ref(), spent.cost.as_ref()]);
        }
        None => spenders.push(Spender {
            label: task.agent.clone(),
            tasks: 1,
            usage: spent.usage.clone(),
            cost: spent.cost.clone(),
        }),
    }
}
