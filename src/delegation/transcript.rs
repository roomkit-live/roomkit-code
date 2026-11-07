//! A delegated task's conversation as text, for `/tasks` (HITL a0875053,
//! option 1): what its agent was handed and said, and each tool it called
//! with how the call stands, the latest last. No view shows a task's room
//! yet; this lets the person follow the work.

use std::collections::HashSet;

use roomkit::model::{EventContent, EventType, RoomEvent, RoomId, ToolCallContent};
use roomkit::model::{ToolCallOutcome, ToolCallStatus};
use roomkit::{GetTimelineOptions, RoomKit};
use roomkit_sqlite::SqliteStore;

/// How many of the room's latest events are read.
const EVENTS: u64 = 200;

/// How many characters of a message are shown.
const MESSAGE_CHARS: usize = 600;

/// How many characters of a call or a result line are shown.
const LINE_CHARS: usize = 120;

/// How many bytes of the latest lines the panel shows, well within what the
/// terminal takes of a panel's text.
const TEXT_BYTES: usize = 24 * 1024;

/// The channel a task's own message comes from.
const SYSTEM: &str = "system";

/// The latest of task room `room`'s conversation, its agent named `agent`.
pub(crate) async fn read(
    kit: &RoomKit<SqliteStore>,
    room: &str,
    agent: &str,
    organization: Option<String>,
) -> String {
    let Ok(id) = RoomId::new(room) else {
        return "The task's conversation cannot be read.\n".into();
    };
    let options = GetTimelineOptions {
        limit: EVENTS,
        newest_first: true,
        organization_id: organization,
        ..Default::default()
    };
    match kit.get_timeline(&id, options).await {
        Ok(mut events) => {
            events.sort_by_key(|event| event.index);
            render(&events, agent)
        }
        Err(_) => "The task's conversation cannot be read.\n".into(),
    }
}

/// The latest lines of `events` within [`TEXT_BYTES`], oldest first: a call
/// that ended is shown once, as it ended.
fn render(events: &[RoomEvent], agent: &str) -> String {
    let ended: HashSet<&str> = events
        .iter()
        .filter(|event| event.r#type == EventType::ToolCallEnd)
        .filter_map(|event| call(event).map(|call| call.tool_id.as_str()))
        .collect();
    let lines: Vec<String> = events
        .iter()
        .filter_map(|event| line(event, agent, &ended))
        .collect();
    if lines.is_empty() {
        return "Nothing yet.\n".into();
    }
    let mut kept = Vec::new();
    let mut bytes = 0;
    for line in lines.iter().rev() {
        bytes += line.len() + 1;
        if bytes > TEXT_BYTES {
            kept.push("(earlier lines not shown)");
            break;
        }
        kept.push(line.as_str());
    }
    kept.reverse();
    let mut text = kept.join("\n");
    text.push('\n');
    text
}

fn line(event: &RoomEvent, agent: &str, ended: &HashSet<&str>) -> Option<String> {
    match event.r#type {
        EventType::Message => {
            let text = message(&event.content);
            if text.trim().is_empty() {
                return None;
            }
            let who = if event.source.channel_id.as_str() == SYSTEM {
                "Task"
            } else {
                agent
            };
            Some(format!("{who}: {}", clip(text.trim(), MESSAGE_CHARS)))
        }
        EventType::ToolCallStart => {
            let call = call(event)?;
            (!ended.contains(call.tool_id.as_str())).then(|| format!("… {}", label(call)))
        }
        EventType::ToolCallEnd => {
            let call = call(event)?;
            Some(ended_line(call))
        }
        _ => None,
    }
}

fn call(event: &RoomEvent) -> Option<&ToolCallContent> {
    match &event.content {
        EventContent::ToolCall(call) => Some(call),
        _ => None,
    }
}

/// What the call did, in its intent's words when the model gave one.
fn label(call: &ToolCallContent) -> String {
    let label = match call
        .intent
        .as_deref()
        .filter(|intent| !intent.trim().is_empty())
    {
        Some(intent) => format!("{} · {intent}", call.tool_name),
        None => {
            let arguments = serde_json::Value::Object(call.arguments.clone()).to_string();
            format!("{} {arguments}", call.tool_name)
        }
    };
    clip(&one_line(&label), LINE_CHARS)
}

fn ended_line(call: &ToolCallContent) -> String {
    let label = label(call);
    let first = |text: &str| clip(&one_line(text), LINE_CHARS);
    match (call.status, call.outcome) {
        (_, Some(ToolCallOutcome::Cancelled)) => format!("− {label} · cancelled"),
        (_, Some(ToolCallOutcome::Refused)) if !call.refused_but_ran => {
            format!("⊘ {label} · refused: {}", first(&result(call)))
        }
        (ToolCallStatus::Completed, Some(ToolCallOutcome::Served)) => format!("✓ {label}"),
        _ => format!("× {label} · failed: {}", first(&result(call))),
    }
}

/// The call's error, else its result, as text.
fn result(call: &ToolCallContent) -> String {
    if let Some(error) = &call.error {
        return error.clone();
    }
    match &call.result {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn message(content: &EventContent) -> String {
    match content {
        EventContent::Text(text) => text.body.clone(),
        EventContent::Rich(text) => text.body.clone(),
        EventContent::Composite(parts) => parts
            .parts()
            .iter()
            .map(message)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let cut: String = text.chars().take(limit - 1).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
    use roomkit::model::{ChannelId, ChannelType, EventSource, TextContent};

    use super::*;

    fn event(channel: &str, r#type: EventType, content: EventContent) -> RoomEvent {
        let source = EventSource::new(ChannelId::new(channel).unwrap(), ChannelType::AI);
        let mut event = RoomEvent::new(RoomId::new("main::t").unwrap(), source, content);
        event.r#type = r#type;
        event
    }

    fn call(id: &str, ended: Option<(ToolCallStatus, ToolCallOutcome)>) -> EventContent {
        let mut call = ToolCallContent::new("shell", id);
        call.arguments.insert("command".into(), "cargo test".into());
        call.intent = (id == "c2").then(|| "Run the tests".into());
        if let Some((status, outcome)) = ended {
            call.status = status;
            call.outcome = Some(outcome);
            call.error = Some("needs the person's approval\nmore".into());
        }
        EventContent::ToolCall(call)
    }

    /// The task, the agent's words and each call once, as it stands: a call
    /// that ended reads how, one that runs reads so; instructions are left out.
    #[test]
    fn a_task_s_conversation_reads_each_call_once_as_it_stands() {
        let text = |body: &str| EventContent::Text(TextContent::new(body));
        let refused = Some((ToolCallStatus::Failed, ToolCallOutcome::Refused));
        let events = [
            event("system", EventType::Message, text("Review the diff")),
            event(
                "agent-3",
                EventType::InstructionRead,
                text("Application mode"),
            ),
            event("agent-3", EventType::ToolCallStart, call("c1", None)),
            event("agent-3", EventType::ToolCallEnd, call("c1", refused)),
            event("agent-3", EventType::Message, text("I could not run it.")),
            event("agent-3", EventType::ToolCallStart, call("c2", None)),
        ];
        assert_eq!(
            render(&events, "Review"),
            "Task: Review the diff\n\
             ⊘ shell {\"command\":\"cargo test\"} · refused: needs the person's approval more\n\
             Review: I could not run it.\n\
             … shell · Run the tests\n"
        );
        assert_eq!(render(&[], "Review"), "Nothing yet.\n");
    }

    /// A long conversation shows its latest lines only, and says so.
    #[test]
    fn a_long_conversation_shows_its_latest_lines() {
        let events: Vec<RoomEvent> = (0..200)
            .map(|n| {
                let body = format!("{n:03} {}", "word ".repeat(100));
                let text = EventContent::Text(TextContent::new(body));
                event("agent-3", EventType::Message, text)
            })
            .collect();
        let text = render(&events, "Review");
        assert!(text.len() <= TEXT_BYTES + 32, "{}", text.len());
        assert!(text.starts_with("(earlier lines not shown)\nReview: "));
        assert!(text.contains("Review: 199 ") && !text.contains("Review: 000 "));
    }
}
