#![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
use super::*;
use roomkit::model::{ChannelId, ChannelType, EventSource, Room, TextContent};
use roomkit_ai::testing::{ScriptedProvider, ScriptedRound};

fn said(index: u64, channel: &str, usage: Option<u64>) -> RoomEvent {
    let source = EventSource::new(ChannelId::new(channel).unwrap(), ChannelType::AI);
    let mut event = RoomEvent::new(
        RoomId::new("room").unwrap(),
        source,
        TextContent::new("Text."),
    );
    event.index = index;
    if let Some(input) = usage {
        let usage = serde_json::json!({"input_tokens": input, "cache_read_input_tokens": 1_000});
        event.metadata.insert("ai_last_usage".into(), usage);
    }
    event
}

/// A request the provider measured past the estimate counts toward the
/// tier, unless it was sent before the latest compaction: it measured
/// the history that compaction cut, and would compact it again.
#[test]
fn the_latest_measured_request_since_the_compaction_counts() {
    let events = [
        said(1, "terminal", None),
        said(2, "code", Some(50_000)),
        said(3, "terminal", None),
    ];
    let none = TurnFootprint::new(0, 0);
    let measured = measure(&events, ("code", 0), &none);
    let since: usize = measured.costs[1..].iter().sum();
    assert_eq!(measured.last, Some(51_000 + since));
    assert_eq!(measured.now(None), 51_000 + since);
    assert!(tiers::due(measured.now(None), 60_000));
    // Measured at or before the compaction's newest event: estimated.
    let stale = measure(&events, ("code", 2), &none);
    assert_eq!(stale.last, None);
    assert!(!tiers::due(stale.now(None), 60_000));
}

/// After a restart no turn measured the rest of the request: the latest
/// measured one says what it took besides the history.
#[test]
fn the_rest_of_a_request_is_read_from_the_latest_measured_one() {
    let events = [
        said(1, "terminal", None),
        said(2, "code", Some(50_000)),
        said(3, "terminal", None),
    ];
    let none = TurnFootprint::new(0, 0);
    let measured = measure(&events, ("code", 0), &none);
    let history: usize = measured.costs.iter().sum();
    assert_eq!(measured.fixed_measured(None), 51_000 - measured.costs[0]);
    assert!(history < 100, "{history}");
    assert_eq!(measure(&events, ("code", 2), &none).fixed_measured(None), 0);
}

/// A turn's context holds the room's latest `WINDOW` rows. Short
/// messages filled them long before their tokens reached the tier: the
/// oldest then left the agent's history with no summary standing for
/// them. The history now compacts past the tier of those rows too, and
/// keeps whole no more rows than a compaction's share of them.
#[tokio::test]
async fn a_history_compacts_before_its_oldest_rows_leave_the_context() {
    let dir = tempfile::tempdir().unwrap();
    let compactions = Arc::new(Compactions::new(dir.path()));
    let provider = ScriptedProvider::new([ScriptedRound::answering("They talked.")]);
    let memory = TieredMemory::new(provider, (1_000_000, "code"), compactions.clone());
    let rows = 1_700;
    let mut context = RoomContext::new(Room::new(RoomId::new("room").unwrap()));
    context.recent_events = (0..rows)
        .map(|index| {
            let own = index % 2 == 1;
            let mut event = said(index, if own { "code" } else { "terminal" }, None);
            if own {
                let ended = serde_json::json!("complete");
                event.metadata.insert(crate::turn_ends::END.into(), ended);
            }
            event
        })
        .collect();
    let current = said(rows, "terminal", None);
    let read = memory.read(("room", "code"), &current, &context).await;
    assert_eq!(memory.provider.requests().len(), 1, "a summary was asked");
    let compacted = compactions.get("room", "code").unwrap();
    assert!(
        rows - compacted.from <= tiers::kept_rows(),
        "{}",
        compacted.from
    );
    assert_eq!(read.events.first().map(|e| e.index), Some(compacted.from));
    assert_eq!(read.messages.len(), 1, "the summary stands for the rest");
}

/// `agent`'s history of `count` turns, from index 1: a message, then
/// the agent's answer, which ends its turn.
fn turns(count: u64) -> Vec<RoomEvent> {
    (1..=count * 2)
        .map(|index| {
            let own = index % 2 == 0;
            let mut event = said(index, if own { "code" } else { "terminal" }, None);
            if own {
                let ended = serde_json::json!("complete");
                event.metadata.insert(crate::turn_ends::END.into(), ended);
            }
            event
        })
        .collect()
}

/// After a relaunch no turn told the project's snapshot, nor measured
/// what a request takes besides its history: `/compact` then wrote a
/// summary without the snapshot and told a size short of the system
/// prompt and the tools. It now carries the previous compaction's.
#[tokio::test]
async fn a_compaction_after_a_relaunch_carries_the_previous_snapshot_and_rest() {
    let dir = tempfile::tempdir().unwrap();
    let previous = Compacted {
        from: 0,
        summary: "Earlier.".into(),
        snapshot: Some("PROJECT RULES".into()),
        summarized: 1,
        at: 0,
        recorded: None,
        estimate: None,
        fixed: Some(30_000),
    };
    let told = ((1, 1), std::time::Duration::ZERO, false);
    Compactions::new(dir.path())
        .record(("room", "code"), (previous, 1), told, None)
        .await;
    let relaunched = Arc::new(Compactions::new(dir.path()));
    let provider = ScriptedProvider::new([ScriptedRound::answering("They talked.")]);
    let memory = TieredMemory::new(provider, (100_000, "code"), relaunched.clone());
    let history = History {
        events: turns(6),
        replayed: Vec::new(),
    };
    memory.compact_now("room", history).await;
    let made = relaunched.get("room", "code").unwrap();
    assert_ne!(made.summary, "Earlier.", "it compacted");
    assert_eq!(made.snapshot.as_deref(), Some("PROJECT RULES"));
    assert!(made.estimate.unwrap().tokens > 30_000);
    assert_eq!(made.fixed, Some(30_000));
}

/// The text of the request a summary was asked with.
fn asked(request: &roomkit_ai::AiContext) -> String {
    match &request.messages[0].content {
        roomkit_ai::AiMessageContent::Text(text) => text.clone(),
        _ => String::new(),
    }
}

/// Review finding: a summary read the newest lines that fit its request
/// and said how many older ones it left out, and the compaction dropped
/// them all from the history: the user's first constraint reached no
/// summary and no request again. Each line now reaches a summary: what
/// does not fit one request is summarized in the next, after the summary
/// of what came before.
#[tokio::test]
async fn a_history_longer_than_a_summary_reads_is_summarized_in_parts() {
    let dir = tempfile::tempdir().unwrap();
    let compactions = Arc::new(Compactions::new(dir.path()));
    let answers = (1..=10).map(|n| ScriptedRound::answering(format!("Summary {n}.")));
    let provider = ScriptedProvider::new(answers);
    let ceiling = 16_000;
    let memory = TieredMemory::new(provider, (ceiling, "code"), compactions.clone());
    let mut events = turns(30);
    for event in &mut events {
        let said = format!("Message {}: {}", event.index, "x".repeat(1_000));
        event.content = TextContent::new(said).into();
    }
    events[0].content = TextContent::new("CONSTRAINT: never touch main.rs.").into();
    let history = History {
        events,
        replayed: Vec::new(),
    };
    memory.compact_now("room", history).await;
    let made = compactions.get("room", "code").unwrap();
    let requests: Vec<String> = memory.provider.requests().iter().map(asked).collect();
    assert!(requests.len() > 1, "{requests:#?}");
    assert!(requests[0].contains("CONSTRAINT: never touch main.rs."));
    // Every message summarized reached a request, within what one reads.
    for index in 2..made.from {
        let message = format!("Message {index}: ");
        assert!(
            requests.iter().any(|asked| asked.contains(&message)),
            "{index}"
        );
    }
    for (at, asked) in requests.iter().enumerate() {
        assert!(!asked.contains("left out"), "{asked}");
        assert!(
            asked.len() <= tiers::summary_chars(ceiling),
            "{}",
            asked.len()
        );
        if at > 0 {
            assert!(asked.contains(&format!("Summary {at}.")), "{asked}");
        }
    }
    assert_eq!(made.summary, format!("Summary {}.", requests.len()));
}

#[tokio::test]
async fn a_local_window_filled_by_fixed_input_warns_even_with_nothing_to_compact() {
    use roomkit_providers_llamacpp::{LlamaCpp, LlamaCppConfig};
    let dir = tempfile::tempdir().unwrap();
    let compactions = Arc::new(Compactions::new(dir.path()));
    let local = LlamaCpp::new(LlamaCppConfig::new("fixture")).unwrap();
    let memory = TieredMemory::new(local, (16_384, "code"), compactions.clone());
    let measured = Measured {
        fixed: 17_020,
        costs: vec![],
        last: None,
    };
    let result = memory
        .compact(
            ("room", "code"),
            None,
            &mut Vec::new(),
            &measured,
            (0, true),
        )
        .await;
    assert!(result.is_none());
    let warnings = compactions.notices("room").join(" ");
    assert!(
        warnings.contains("17020 tokens against a 16384-token context")
            && warnings.contains("/agents"),
        "{warnings}"
    );
}
