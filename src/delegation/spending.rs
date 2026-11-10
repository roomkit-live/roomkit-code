//! What a conversation's delegated tasks consumed (RMK-1536, HITL a0ea8a5f):
//! read from each task's room once it ended, shown task by task, and added to
//! what the conversation's own agent consumed. A worker's turns record their
//! usage when they end, so a task that runs has no figure yet.

use std::iter::once;

use roomkit::RoomKit;
use roomkit::model::{ChannelId, RoomId};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::ConversationCost;
use roomkit_tui_engine::SessionConfig;

use super::{Task, Tasks};
use crate::exit_summary::{Spender, Usage, amount};
use crate::profiles::Profiles;

/// What a task's worker consumed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Spent {
    pub(crate) usage: Usage,
    /// At the list price of the worker's model, when it has one.
    pub(crate) cost: Option<ConversationCost>,
}

impl Spent {
    /// `58k tokens · ≈ $0.61`.
    pub(crate) fn text(&self) -> String {
        let total = self.usage.input.saturating_add(self.usage.output);
        let tokens = crate::compaction::tokens(usize::try_from(total).unwrap_or(usize::MAX));
        let cost = self
            .cost
            .as_ref()
            .map_or_else(String::new, |cost| format!(" · {}", amount(cost)));
        let whole = self.usage.complete && self.cost.as_ref().is_none_or(|cost| cost.complete);
        let partial = if whole { "" } else { " (partial)" };
        format!("{tokens} tokens{cost}{partial}")
    }
}

/// Reads what the ended tasks of `current`'s conversation consumed, those
/// not read yet or since a turn was recorded there (`Tasks::consumed`).
pub(crate) async fn read(
    kit: &RoomKit<SqliteStore>,
    profiles: &Profiles,
    tasks: &Tasks,
    current: &SessionConfig,
) {
    for (_, task) in tasks.family(current.room.as_str()) {
        if task.running() || task.spent.is_some() {
            continue;
        }
        let organization = current.organization.clone();
        let version = tasks.version();
        if let Some(spent) = spent(kit, profiles, &task, organization).await {
            tasks.keep_spent(&task.id, spent, version);
        }
    }
}

/// What the room of `task` records its worker consumed, priced as the
/// profiles now price that agent; none when the room cannot be read, which a
/// later view tries again.
async fn spent(
    kit: &RoomKit<SqliteStore>,
    profiles: &Profiles,
    task: &Task,
    organization: Option<String>,
) -> Option<Spent> {
    let room = RoomId::new(&task.room).ok()?;
    let agent = ChannelId::new(&task.agent_id).ok()?;
    let mut usage = crate::exit_summary::usage(kit, &room).await.ok()?;
    usage.complete &= !task.cut_short();
    let (cost, _) = priced(kit, profiles, &room, &agent, organization, usage.complete).await?;
    Some(Spent { usage, cost })
}

/// What `agent`'s turns in `room` cost at the list price of its model, with
/// the view that priced them: the cost is partial when its usage is not
/// `complete`, and none when its model has no price. None when the room
/// cannot be read.
async fn priced(
    kit: &RoomKit<SqliteStore>,
    profiles: &Profiles,
    room: &RoomId,
    agent: &ChannelId,
    organization: Option<String>,
    complete: bool,
) -> Option<(Option<ConversationCost>, SessionConfig)> {
    let mut config = SessionConfig::new(room.clone(), agent.clone(), "local-user");
    config.organization = organization;
    profiles.configure_view(&mut config).await;
    let cost = roomkit_tui_engine::conversation_cost(kit, &config)
        .await
        .ok()?;
    let cost = cost.map(|mut cost| {
        cost.complete &= complete;
        cost
    });
    Some((cost, config))
}

/// Each agent's part of the conversation's own room, in the order they first
/// answered: an agent may have been changed during the conversation. One whose
/// cost could not be read keeps its line, with no cost, which leaves the total
/// partial.
pub(crate) async fn leads(
    kit: &RoomKit<SqliteStore>,
    profiles: &Profiles,
    config: &SessionConfig,
) -> Vec<Spender> {
    let Ok(agents) = crate::exit_summary::usage_by_agent(kit, &config.room).await else {
        return Vec::new();
    };
    let mut leads = Vec::new();
    for (agent, usage) in agents {
        let organization = config.organization.clone();
        let complete = usage.complete;
        let room = &config.room;
        let read = priced(kit, profiles, room, &agent, organization, complete).await;
        let (cost, view) = read.unzip();
        let label = view
            .as_ref()
            .and_then(|view| {
                let names = view.actor_names.iter();
                names
                    .into_iter()
                    .find(|(id, _)| id.as_str() == agent.as_str())
            })
            .map_or_else(|| agent.to_string(), |(_, label)| label.clone());
        leads.push(Spender::lead(
            agent.to_string(),
            label,
            usage,
            cost.flatten(),
        ));
    }
    leads
}

/// What the ended tasks of `current`'s conversation cost in its currency,
/// which the view adds to the conversation's own cost; none before one ended
/// or when nothing has a price.
pub(crate) fn related(tasks: &Tasks, current: &SessionConfig) -> Option<ConversationCost> {
    let listed = tasks.family(current.room.as_str());
    let ended: Vec<&Task> = listed
        .iter()
        .map(|(_, task)| task)
        .filter(|task| !task.running())
        .collect();
    if ended.is_empty() {
        return None;
    }
    let own = current
        .pricing
        .as_ref()
        .map(|pricing| ConversationCost::new(0, pricing.currency.clone(), true));
    let costs = ended.iter().map(|task| cost_of(task));
    sum(once(own.as_ref())
        .filter(|cost| cost.is_some())
        .chain(costs))
}

/// What the rest of the conversation cost, beside a task's room `current`
/// shows: the conversation's own agent, and its other tasks.
pub(crate) async fn around_task(
    kit: &RoomKit<SqliteStore>,
    profiles: &Profiles,
    tasks: &Tasks,
    current: &SessionConfig,
    root: &str,
) -> Option<ConversationCost> {
    let own = own_cost(kit, profiles, tasks, current, root).await;
    let listed = tasks.family(current.room.as_str());
    let others = listed
        .iter()
        .map(|(_, task)| task)
        .filter(|task| !task.running() && task.room != current.room.as_str());
    let costs = others.map(cost_of);
    sum(once(own.as_ref()).chain(costs))
}

/// What the agent of conversation `root` cost, read once for each change to
/// the figures: a view polling a task's room would read the whole conversation.
async fn own_cost(
    kit: &RoomKit<SqliteStore>,
    profiles: &Profiles,
    tasks: &Tasks,
    current: &SessionConfig,
    root: &str,
) -> Option<ConversationCost> {
    let version = tasks.version();
    if let Some(cost) = tasks.own_cost(root, version) {
        return cost;
    }
    let room = RoomId::new(root).ok()?;
    let agent = ChannelId::new(tasks.lead_of(root)?).ok()?;
    let organization = current.organization.clone();
    let (cost, _) = priced(kit, profiles, &room, &agent, organization, true).await?;
    tasks.keep_own_cost(root, version, cost.clone());
    cost
}

/// What a task cost, if that is known.
pub(crate) fn cost_of(task: &Task) -> Option<&ConversationCost> {
    task.spent.as_ref()?.cost.as_ref()
}

/// `costs` added up in the first one's currency. One that is unknown or in
/// another currency cannot be added: the sum then reads partial.
pub(crate) fn sum<'a>(
    costs: impl IntoIterator<Item = Option<&'a ConversationCost>>,
) -> Option<ConversationCost> {
    let (mut total, mut whole) = (None::<ConversationCost>, true);
    for cost in costs {
        match (cost, total.as_mut()) {
            (None, _) => whole = false,
            (Some(cost), None) => total = Some(cost.clone()),
            (Some(cost), Some(sum)) if cost.currency == sum.currency => {
                sum.micros = sum.micros.saturating_add(cost.micros);
                sum.complete &= cost.complete;
            }
            (Some(_), Some(_)) => whole = false,
        }
    }
    total.map(|mut total| {
        total.complete &= whole;
        total
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
    use super::*;

    fn usd(micros: u64, complete: bool) -> ConversationCost {
        ConversationCost::new(micros, "USD", complete)
    }

    #[test]
    fn costs_add_up_in_the_first_currency_and_what_cannot_be_added_reads_partial() {
        let (a, b) = (usd(1_000, true), usd(2_000, true));
        assert_eq!(sum([Some(&a), Some(&b)]), Some(usd(3_000, true)));
        assert_eq!(sum([Some(&a), None]), Some(usd(1_000, false)));
        let euro = ConversationCost::new(5, "EUR", true);
        assert_eq!(sum([Some(&a), Some(&euro)]), Some(usd(1_000, false)));
        assert_eq!(
            sum([Some(&a), Some(&usd(1, false))]),
            Some(usd(1_001, false))
        );
        // Unknown first: what follows still sets the currency.
        assert_eq!(sum([None, Some(&a)]), Some(usd(1_000, false)));
        assert_eq!(sum([None]), None);
    }

    fn task(id: &str, ended: bool, cost: Option<ConversationCost>) -> Task {
        Task {
            id: id.into(),
            parent: "main".into(),
            room: format!("main::{id}"),
            agent: "Review".into(),
            agent_id: "review".into(),
            input: "Task".into(),
            started: std::time::Instant::now(),
            ended: ended.then_some((
                std::time::Duration::from_secs(1),
                crate::delegation::Outcome::Completed,
            )),
            spent: Some(spent(10, 1, true, cost)),
        }
    }

    fn view(priced: bool) -> SessionConfig {
        let room = RoomId::new("main").unwrap();
        let mut view = SessionConfig::new(room, ChannelId::new("code").unwrap(), "local-user");
        let verified = roomkit_ai::Date::new(2026, 11, 6).unwrap();
        view.pricing = priced.then(|| roomkit_ai::ModelPricing::new(2.0, 10.0, verified).unwrap());
        view
    }

    fn board(tasks: Vec<Task>) -> Tasks {
        let board = Tasks::default();
        board.restored(tasks);
        board
    }

    #[test]
    fn the_related_cost_adds_the_ended_tasks_once_one_ended() {
        let quiet = board(vec![task("a", false, None)]);
        assert_eq!(related(&quiet, &view(true)), None);

        let priced = board(vec![
            task("a", true, Some(usd(1_000, true))),
            task("b", true, Some(usd(2_000, true))),
            task("running", false, None),
        ]);
        assert_eq!(related(&priced, &view(true)), Some(usd(3_000, true)));
        assert_eq!(related(&priced, &view(false)), Some(usd(3_000, true)));

        // A task with no price leaves it partial; in the conversation's
        // currency even when no task has a price.
        let unpriced = board(vec![task("a", true, None)]);
        assert_eq!(related(&unpriced, &view(true)), Some(usd(0, false)));
        assert_eq!(related(&unpriced, &view(false)), None);
    }

    fn spent(input: u64, output: u64, complete: bool, cost: Option<ConversationCost>) -> Spent {
        Spent {
            usage: Usage {
                input,
                cached: 0,
                output,
                turns: 1,
                complete,
                last_turn_recorded: true,
            },
            cost,
        }
    }

    #[test]
    fn a_task_reads_its_tokens_and_cost() {
        let priced = spent(57_000, 1_000, true, Some(usd(610_000, true)));
        assert_eq!(priced.text(), "58k tokens · ≈ $0.61");
        assert_eq!(spent(800, 50, true, None).text(), "850 tokens");
        assert_eq!(
            spent(800, 50, false, Some(usd(610_000, true))).text(),
            "850 tokens · ≈ $0.61 (partial)"
        );
        assert_eq!(
            spent(800, 50, true, Some(usd(610_000, false))).text(),
            "850 tokens · ≈ $0.61 (partial)"
        );
    }
}
