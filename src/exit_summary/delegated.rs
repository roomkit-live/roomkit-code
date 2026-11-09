//! The exit report of a conversation whose agents handed tasks over: what
//! the conversation consumed as a whole, then each agent's part.

use roomkit_tui::protocol::ConversationCost;

use super::{Summary, Usage, amount};
use crate::delegation::{Task, spending};

/// One agent's part of a conversation's consumption.
#[derive(Debug)]
pub(crate) struct Spender {
    agent: String,
    label: String,
    /// How many delegated tasks it worked on; 0 for its part of the
    /// conversation's own room.
    tasks: usize,
    usage: Usage,
    cost: Option<ConversationCost>,
}

impl Spender {
    /// What `agent` consumed in the conversation's own room.
    pub(crate) fn lead(
        agent: String,
        label: String,
        usage: Usage,
        cost: Option<ConversationCost>,
    ) -> Self {
        Self {
            agent,
            label,
            tasks: 0,
            usage,
            cost,
        }
    }

    pub(crate) fn is_agent(&self, agent: &str) -> bool {
        self.agent == agent
    }

    /// `  Review (2 tasks): total=18000 · 2 turns · ≈ $0.88`.
    fn line(&self) -> String {
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

    fn cut(&mut self) {
        self.usage.complete = false;
        if let Some(cost) = &mut self.cost {
            cost.complete = false;
        }
    }
}

impl Summary {
    /// Adds what the conversation's `tasks` consumed to its report: its usage
    /// and cost become the whole conversation's, `leads` give each agent's
    /// part of its own room, and each worker gets a line. What is unknown
    /// leaves the total partial: a task that still runs or could not be read,
    /// a turn of `shown` closing cuts, or a room that could not be read.
    pub(crate) fn add_tasks(&mut self, mut leads: Vec<Spender>, tasks: &[Task], shown: &str) {
        let Some(usage) = self.usage.as_mut() else {
            return;
        };
        // With tasks to add, a cut turn misstates the report even when it is
        // the room's first.
        let cut = self.turn_left_running && !usage.last_turn_recorded;
        if cut && let Some(lead) = leads.iter_mut().find(|lead| lead.is_agent(shown)) {
            lead.cut();
        }
        let read: Vec<&Task> = tasks.iter().filter(|task| task.spent.is_some()).collect();
        let whole = read.len() == tasks.len() && !cut && !leads.is_empty();
        usage.complete &= whole;
        // The report's own cost is that of the shown agent alone: without its
        // parts, it is all that is known of the room.
        let own: Vec<_> = if leads.is_empty() {
            vec![self.cost.as_ref()]
        } else {
            leads.iter().map(|lead| lead.cost.as_ref()).collect()
        };
        let costs = read.iter().map(|task| spending::cost_of(task));
        let mut cost = spending::sum(own.into_iter().chain(costs));
        if let Some(cost) = cost.as_mut() {
            cost.complete &= whole;
        }
        self.cost = cost;
        let read_count = read.len();
        for task in read {
            if let Some(spent) = &task.spent {
                usage.absorb(&spent.usage);
                add_to_worker(&mut leads, task);
            }
        }
        self.tasks_uncounted = tasks.len() - read_count;
        self.spenders = leads;
    }

    /// Each agent's part, then how many tasks could not be counted.
    pub(super) fn delegated_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.spenders.iter().map(Spender::line).collect();
        match self.tasks_uncounted {
            0 => {}
            1 => lines.push("  1 delegated task not counted: still running or unreadable".into()),
            count => lines.push(format!(
                "  {count} delegated tasks not counted: still running or unreadable"
            )),
        }
        lines
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
        .find(|spender| spender.tasks > 0 && spender.agent == task.agent_id);
    match line {
        Some(line) => {
            line.tasks += 1;
            line.usage.absorb(&spent.usage);
            line.cost = spending::sum([line.cost.as_ref(), spent.cost.as_ref()]);
        }
        None => spenders.push(Spender {
            agent: task.agent_id.clone(),
            label: task.agent.clone(),
            tasks: 1,
            usage: spent.usage.clone(),
            cost: spent.cost.clone(),
        }),
    }
}
