//! What the terminal prints once the view has closed: how to reopen the
//! conversation, its consumption over every launch, and the background
//! processes that closing the application stopped.

use std::time::Duration;

use roomkit::model::{ChannelId, EventType, Metadata, RoomEvent, RoomId};
use roomkit::{GetTimelineOptions, RoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tools_process::{ProcessSession, ProcessState};
use roomkit_tui::protocol::ConversationCost;
use roomkit_tui_engine::SessionConfig;
use serde_json::{Value, json};

mod delegated;

pub(crate) use delegated::Spender;

/// Processes listed by name; the rest are counted.
const LISTED: usize = 8;
const TIMELINE_PAGE: u64 = 500;

/// Reported consumption summed over every turn of a conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Usage {
    /// Input tokens the provider did not serve from its cache.
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    pub turns: u64,
    /// False when a turn lacked a complete report.
    pub complete: bool,
    /// Whether the conversation's last turn holds its record on a row.
    pub last_turn_recorded: bool,
}

/// A process that was running when the application began to close.
#[derive(Debug)]
pub(crate) struct Process {
    pub id: String,
    pub command: String,
    pub elapsed: Duration,
}

/// Facts gathered while the store and the processes are still open.
#[derive(Debug, Default)]
pub(crate) struct Summary {
    /// The conversation shown at exit, when it has history to resume.
    pub session: Option<String>,
    pub resume: Option<Vec<String>>,
    pub usage: Option<Usage>,
    /// The conversation's cost at its model's list price, when it has one.
    pub cost: Option<ConversationCost>,
    /// Each agent's part, when some worked on delegated tasks.
    pub spenders: Vec<Spender>,
    /// How many delegated tasks still ran as the report was gathered.
    pub tasks_running: usize,
    /// The view left a turn running, which closing cuts.
    pub turn_cut: bool,
    pub running: Vec<Process>,
}

impl Summary {
    /// Only the processes still running: the report of a launch that ended
    /// before it could gather the rest, which still says what it leaves.
    pub(crate) async fn of_processes(processes: &ProcessSession) -> Self {
        Self {
            session: None,
            resume: None,
            usage: None,
            cost: None,
            spenders: Vec::new(),
            tasks_running: 0,
            turn_cut: false,
            running: running(processes).await,
        }
    }
}

/// Gather the report for the conversation shown at exit, given with the
/// command that reopens it, and `priced`, its view priced as the profiles
/// price it. An untouched draft has nothing to resume.
pub(crate) async fn collect(
    kit: &RoomKit<SqliteStore>,
    processes: &ProcessSession,
    shown: Option<(String, Option<Vec<String>>)>,
    priced: Option<SessionConfig>,
) -> Summary {
    let mut summary = Summary {
        running: running(processes).await,
        ..Summary::default()
    };
    let Some((session, resume)) = shown else {
        return summary;
    };
    let Ok(room) = RoomId::new(&session) else {
        return summary;
    };
    if crate::sessions::empty(kit, &room).await.unwrap_or(true) {
        return summary;
    }
    summary.usage = usage(kit, &room).await.ok();
    if let Some(config) = priced.filter(|config| config.room == room) {
        summary.cost = roomkit_tui_engine::conversation_cost(kit, &config)
            .await
            .ok()
            .flatten();
    }
    // The ID still reopens it; a name only helps recognize it.
    summary.session = Some(match crate::sessions::name(kit, &room).await {
        Some(name) => format!("{name} · {session}"),
        None => session,
    });
    summary.resume = resume;
    summary
}

async fn running(processes: &ProcessSession) -> Vec<Process> {
    processes
        .observe()
        .await
        .into_iter()
        .filter(|process| process.status.state == ProcessState::Running)
        .map(|process| Process {
            id: process.status.process_id.to_string(),
            command: process.command,
            elapsed: process.elapsed,
        })
        .collect()
}

/// Sum every turn's reported totals over the whole conversation. The engine
/// stores one `ai_usage` record per turn, so no model call is counted twice.
pub(crate) async fn usage(kit: &RoomKit<SqliteStore>, room: &RoomId) -> crate::Result<Usage> {
    let mut usage = Usage::none();
    each_row(kit, room, |event| usage.add(event)).await?;
    Ok(usage)
}

/// The same totals, for each agent that answered in `room`.
pub(crate) async fn usage_by_agent(
    kit: &RoomKit<SqliteStore>,
    room: &RoomId,
) -> crate::Result<Vec<(ChannelId, Usage)>> {
    let mut agents: Vec<(ChannelId, Usage)> = Vec::new();
    each_row(kit, room, |event| {
        let source = &event.source.channel_id;
        let index = agents.iter().position(|(agent, _)| agent == source);
        let index = index.unwrap_or_else(|| {
            agents.push((source.clone(), Usage::none()));
            agents.len() - 1
        });
        agents[index].1.add(event);
    })
    .await?;
    // A person's messages hold no turn.
    agents.retain(|(_, usage)| usage.turns > 0 || !usage.complete);
    Ok(agents)
}

/// Hands `read` every row of `room`, the oldest first.
async fn each_row(
    kit: &RoomKit<SqliteStore>,
    room: &RoomId,
    mut read: impl FnMut(&RoomEvent),
) -> crate::Result<()> {
    let mut after = None;
    loop {
        let options = GetTimelineOptions {
            limit: TIMELINE_PAGE,
            after_index: after,
            ..Default::default()
        };
        let page = kit.get_timeline(room, options).await?;
        let Some(last) = page.last() else {
            return Ok(());
        };
        after = Some(last.index);
        page.iter().for_each(&mut read);
    }
}

impl Usage {
    /// Nothing counted yet.
    fn none() -> Self {
        Self {
            input: 0,
            cached: 0,
            output: 0,
            turns: 0,
            complete: true,
            last_turn_recorded: false,
        }
    }

    /// Counts `other` in these totals.
    fn absorb(&mut self, other: &Self) {
        self.input = self.input.saturating_add(other.input);
        self.cached = self.cached.saturating_add(other.cached);
        self.output = self.output.saturating_add(other.output);
        self.turns = self.turns.saturating_add(other.turns);
        self.complete &= other.complete;
    }

    fn add(&mut self, event: &RoomEvent) {
        self.read(matches!(event.r#type, EventType::Message), &event.metadata);
    }

    fn read(&mut self, message: bool, metadata: &Metadata) {
        // A turn that wrote no message holds its record on its last row.
        let recorded = metadata.contains_key("loop_end_reason");
        // A `!` command's row is no turn's, whatever it says.
        if (message || recorded) && !metadata.keys().any(|key| key.starts_with("shell_")) {
            self.last_turn_recorded = recorded;
        }
        let Some(total) = metadata
            .get("ai_usage")
            .and_then(Value::as_object)
            .filter(|total| !total.is_empty() && (message || recorded))
        else {
            // A turn stopped before anything told what it used, or a provider
            // that reported nothing: unknown tokens.
            self.complete &= metadata.get("loop_end_reason") != Some(&json!("cancelled"))
                && metadata.get("ai_usage_complete") != Some(&Value::Bool(false));
            return;
        };
        let count = |key| total.get(key).and_then(Value::as_u64);
        let (input, output) = (count("input_tokens"), count("output_tokens"));
        // Since roomkit-rs f86cbb1 a Chat Completions usage reports a prompt's
        // cache writes apart from its `input_tokens`: they are input the cache
        // did not serve (RMK-1457).
        self.input += input.unwrap_or(0) + count("cache_creation_input_tokens").unwrap_or(0);
        self.cached += count("cache_read_input_tokens").unwrap_or(0);
        self.output += output.unwrap_or(0);
        self.turns += 1;
        let reported = metadata.get("ai_usage_complete") == Some(&Value::Bool(true));
        self.complete &= reported && input.is_some() && output.is_some();
    }

    fn line(&self, cost: Option<&ConversationCost>) -> String {
        let cost = cost.map_or_else(String::new, |cost| {
            let partial = if cost.complete { "" } else { " (partial)" };
            format!(" · cost {}{partial}", amount(cost))
        });
        format!(
            "Token usage: total={} input={} (+ {} cached) output={} · {} {}{}{cost}",
            self.input.saturating_add(self.output),
            self.input,
            self.cached,
            self.output,
            self.turns,
            if self.turns == 1 { "turn" } else { "turns" },
            if self.complete { "" } else { " (partial)" },
        )
    }
}

/// `≈ $0.42`, a list price's estimate; below a cent, four decimals, as the
/// view writes it.
pub(crate) fn amount(cost: &ConversationCost) -> String {
    let value = cost.micros as f64 / 1_000_000.0;
    let digits = if cost.micros == 0 || cost.micros >= 10_000 {
        2
    } else {
        4
    };
    let value = format!("{value:.digits$}");
    match cost.currency.as_str() {
        "USD" => format!("≈ ${value}"),
        currency => format!("≈ {value} {currency}"),
    }
}

impl Summary {
    /// The view left a turn running: closing cuts it after this report is
    /// read, so the usage read here is not all of it, unless its record is
    /// written already, its answer done but for the view's last word.
    pub(crate) fn turn_running(&mut self) {
        // With no turn counted there is nothing the report would misstate: the
        // view's run may also be a `!` command's, or a tool server's.
        if let Some(usage) = self
            .usage
            .as_mut()
            .filter(|usage| usage.turns > 0 && !usage.last_turn_recorded)
        {
            usage.complete = false;
            self.turn_cut = true;
        }
    }

    /// Write how to reopen the conversation and what it used to the restored
    /// terminal, before closing begins: a close that takes long, or that a
    /// signal cuts, no longer loses them.
    pub fn print_conversation(&self) {
        for line in self.conversation_report() {
            println!("{line}");
        }
    }

    /// Write the processes closing stopped, once it has.
    pub fn print_processes(&self, stopped: bool) {
        for line in self.process_report(Fate::closed(stopped)) {
            println!("{line}");
        }
    }

    /// The whole report, in the order it is written; `stopped` tells whether
    /// closing confirmed that processes were stopped.
    #[cfg(test)]
    pub fn lines(&self, stopped: bool) -> Vec<String> {
        let mut lines = self.conversation_report();
        lines.extend(self.process_report(Fate::closed(stopped)));
        lines
    }

    /// The processes still running as the application closed, and what
    /// closing did to them.
    pub fn process_report(&self, fate: Fate) -> Vec<String> {
        let mut lines = Vec::new();
        self.processes(&mut lines, fate);
        lines
    }

    /// The conversation's session, how to reopen it, and its usage.
    pub fn conversation_report(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(session) = &self.session {
            lines.push(format!("Session: {session}"));
            if let Some(command) = &self.resume {
                lines.push("To continue this session, run:".into());
                let command: Vec<_> = command.iter().map(|arg| quoted(arg)).collect();
                lines.push(format!("  {}", command.join(" ")));
            }
        }
        if let Some(usage) = self
            .usage
            .as_ref()
            .filter(|usage| usage.turns > 0 || !usage.complete)
        {
            lines.push(usage.line(self.cost.as_ref()));
            lines.extend(self.delegated_lines());
        }
        lines
    }

    fn processes(&self, lines: &mut Vec<String>, fate: Fate) {
        let count = self.running.len();
        if count == 0 {
            return;
        }
        let noun = if count == 1 { "process" } else { "processes" };
        lines.push(match fate {
            Fate::Stopped => format!("Stopped {count} background {noun}:"),
            Fate::Unknown => format!("{count} background {noun} may still be running:"),
            Fate::LeftRunning => format!("Left {count} background {noun} running:"),
        });
        for process in self.running.iter().take(LISTED) {
            lines.push(format!(
                "  {} · {} · {}",
                process.id,
                one_line(&process.command),
                elapsed(process.elapsed)
            ));
        }
        if count > LISTED {
            lines.push(format!("  … {} more", count - LISTED));
        }
    }
}

/// What closing did to the processes still running as the application closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fate {
    /// Closing confirmed they were stopped.
    Stopped,
    /// Closing could not confirm it: they may still run.
    Unknown,
    /// An unattended launch let them go, still running.
    LeftRunning,
}

impl Fate {
    /// What a close that stopped processes reports: `stopped` tells whether
    /// it confirmed so.
    pub fn closed(stopped: bool) -> Self {
        if stopped {
            Self::Stopped
        } else {
            Self::Unknown
        }
    }
}

/// Quote an argument for a POSIX shell only when it needs it.
fn quoted(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(&b));
    if plain {
        arg.into()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

/// A command on one bounded line, without terminal control characters.
fn one_line(command: &str) -> String {
    let text = command
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() > 60 {
        format!("{}…", text.chars().take(59).collect::<String>())
    } else {
        text
    }
}

fn elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match (seconds / 3600, seconds / 60 % 60, seconds % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m {s:02}s"),
        (h, m, _) => format!("{h}h {m:02}m"),
    }
}

#[cfg(test)]
mod tests;
