//! What the terminal prints once the view has closed: how to reopen the
//! conversation, its consumption over every launch, and the background
//! processes that closing the application stopped.

use std::time::Duration;

use roomkit::model::{EventType, RoomEvent, RoomId};
use roomkit::{GetTimelineOptions, RoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tools_process::{ProcessSession, ProcessState};
use serde_json::Value;

/// Processes listed by name; the rest are counted.
const LISTED: usize = 8;
const TIMELINE_PAGE: u64 = 500;

/// Reported consumption summed over every turn of a conversation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Usage {
    /// Input tokens the provider did not serve from its cache.
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    pub turns: u64,
    /// False when a turn lacked a complete report.
    pub complete: bool,
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
    pub running: Vec<Process>,
}

/// Gather the report for the conversation shown at exit, given with the
/// command that reopens it. An untouched draft has nothing to resume.
pub(crate) async fn collect(
    kit: &RoomKit<SqliteStore>,
    processes: &ProcessSession,
    shown: Option<(String, Option<Vec<String>>)>,
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
    let mut usage = Usage {
        input: 0,
        cached: 0,
        output: 0,
        turns: 0,
        complete: true,
    };
    let mut after = None;
    loop {
        let options = GetTimelineOptions {
            limit: TIMELINE_PAGE,
            after_index: after,
            ..Default::default()
        };
        let page = kit.get_timeline(room, options).await?;
        let Some(last) = page.last() else {
            return Ok(usage);
        };
        after = Some(last.index);
        page.iter().for_each(|event| usage.add(event));
    }
}

impl Usage {
    fn add(&mut self, event: &RoomEvent) {
        let Some(total) = event
            .metadata
            .get("ai_usage")
            .and_then(Value::as_object)
            .filter(|total| !total.is_empty() && matches!(event.r#type, EventType::Message))
        else {
            return;
        };
        let count = |key| total.get(key).and_then(Value::as_u64);
        let (input, output) = (count("input_tokens"), count("output_tokens"));
        self.input += input.unwrap_or(0);
        self.cached += count("cache_read_input_tokens").unwrap_or(0);
        self.output += output.unwrap_or(0);
        self.turns += 1;
        let reported = event.metadata.get("ai_usage_complete") == Some(&Value::Bool(true));
        self.complete &= reported && input.is_some() && output.is_some();
    }

    fn line(&self) -> String {
        format!(
            "Token usage: total={} input={} (+ {} cached) output={} · {} {}{}",
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

impl Summary {
    /// Write the report to the restored terminal.
    pub fn print(&self, stopped: bool) {
        for line in self.lines(stopped) {
            println!("{line}");
        }
    }

    /// `stopped` tells whether closing confirmed that processes were stopped.
    pub fn lines(&self, stopped: bool) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(session) = &self.session {
            lines.push(format!("Session: {session}"));
            if let Some(command) = &self.resume {
                lines.push("To continue this session, run:".into());
                let command: Vec<_> = command.iter().map(|arg| quoted(arg)).collect();
                lines.push(format!("  {}", command.join(" ")));
            }
        }
        if let Some(usage) = self.usage.as_ref().filter(|usage| usage.turns > 0) {
            lines.push(usage.line());
        }
        self.processes(&mut lines, stopped);
        lines
    }

    fn processes(&self, lines: &mut Vec<String>, stopped: bool) {
        let count = self.running.len();
        if count == 0 {
            return;
        }
        let noun = if count == 1 { "process" } else { "processes" };
        lines.push(if stopped {
            format!("Stopped {count} background {noun}:")
        } else {
            format!("{count} background {noun} may still be running:")
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
