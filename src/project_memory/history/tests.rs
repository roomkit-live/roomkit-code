#![expect(
    clippy::unwrap_used,
    reason = "Unexpected store or tool errors fail these fixtures"
)]
use super::*;
use roomkit::model::{
    Access, ChannelBinding, ChannelCategory, ChannelType, EventSource, EventStatus, TextContent,
    Visibility,
};
use roomkit_ai::{ToolTurnContextOptions, tool_turn_context};
use roomkit_providers_sqlite::SqliteStoreOptions;

fn room(name: &str) -> Room {
    Room::new(RoomId::new(name).unwrap())
}

async fn bind(store: &SqliteStore, room: &Room, agent: Access) {
    store.create_room(room.clone()).await.unwrap();
    for (name, kind, category, access) in [
        (
            "terminal",
            ChannelType::CLI,
            ChannelCategory::Transport,
            Access::ReadWrite,
        ),
        (
            "code",
            ChannelType::AI,
            ChannelCategory::Intelligence,
            agent,
        ),
    ] {
        let mut binding = ChannelBinding::new(ChannelId::new(name).unwrap(), room.id.clone(), kind);
        binding.category = category;
        binding.access = access;
        store.add_binding(binding).await.unwrap();
    }
}

fn message(room: &Room, text: &str) -> RoomEvent {
    let source = EventSource::new(ChannelId::new("speaker").unwrap(), ChannelType::AI);
    let mut event = RoomEvent::new(room.id.clone(), source, TextContent::new(text));
    event.status = EventStatus::Delivered;
    event
}

async fn append(store: &SqliteStore, event: RoomEvent) -> String {
    let id = event.room_id.clone();
    let row = store
        .commit_event(&id, event, Default::default())
        .await
        .unwrap();
    format!("{}#{}", id, row.index)
}

async fn lookup(
    history: &History,
    caller: &Room,
    query: Option<&str>,
    reference: Option<&str>,
) -> Result<String, ToolError> {
    let turn = ToolTurnContextOptions {
        room: Some(caller.clone().into()),
        ..Default::default()
    };
    tool_turn_context(turn, history.lookup(query, reference, 256))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_citation_reads_the_decision_in_order_and_can_continue_through_a_sparse_timeline() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::new(SqliteStoreOptions {
        path: dir.path().join("store"),
    });
    let history = History::for_agent(Some(&store), "code").unwrap();
    let (caller, old) = (room("follow-up"), room("docs-review"));
    bind(&store, &caller, Access::ReadWrite).await;
    bind(&store, &old, Access::ReadOnly).await;
    append(&store, message(&old, "The crew requested a guide.")).await;
    let hit = append(
        &store,
        message(
            &old,
            "The orchard handbook could be a website or a printed booklet.",
        ),
    )
    .await;
    for _ in 0..40 {
        let mut audit = message(&old, "orchard handbook");
        audit.r#type = EventType::System;
        append(&store, audit).await;
        let mut internal = message(&old, "private draft");
        internal.visibility = Visibility::Internal;
        append(&store, internal).await;
    }
    let decision = append(&store, message(&old, "Decision: print the booklet.")).await;
    let reason = append(&store, message(&old, "The crew has no network access.")).await;
    let followup = append(&store, message(&old, "Ada will review the proof.")).await;
    let hits = lookup(
        &history,
        &caller,
        Some("orchard handbook conversation"),
        None,
    )
    .await
    .unwrap();
    assert!(
        hits.contains("Partial match") && hits.contains(&hit),
        "{hits}"
    );
    let passage = lookup(&history, &caller, None, Some(&hit)).await.unwrap();
    let citations: Vec<_> = passage
        .lines()
        .map(|s| s.split_whitespace().next().unwrap())
        .collect();
    assert_eq!(
        &citations[1..],
        [hit.as_str(), decision.as_str(), reason.as_str()]
    );
    assert!(passage.contains("print the booklet") && passage.contains("no network access"));
    assert!(!passage.contains("private draft"));
    let next = lookup(&history, &caller, None, Some(&reason))
        .await
        .unwrap();
    assert!(next.contains(&followup) && next.contains(&hit), "{next}");
    assert!(
        lookup(&history, &caller, Some("orchard"), Some(&hit))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn guessed_citations_enforce_the_same_scope_and_visibility_as_search() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::new(SqliteStoreOptions {
        path: dir.path().join("store"),
    });
    let history = History::for_agent(Some(&store), "code").unwrap();
    assert!(History::for_agent(None, "code").is_none());
    assert!(History::for_agent(Some(&store), crate::names::REMOTE_AGENT).is_none());
    let caller = room("caller");
    bind(&store, &caller, Access::ReadWrite).await;
    let public = room("public");
    bind(&store, &public, Access::ReadWrite).await;
    let mut hidden = vec![append(&store, message(&caller, "hiddenneedle")).await];
    for (kind, visibility, status) in [
        (EventType::System, Visibility::All, EventStatus::Delivered),
        (EventType::Message, Visibility::All, EventStatus::Blocked),
        (
            EventType::Message,
            Visibility::Transport,
            EventStatus::Delivered,
        ),
        (
            EventType::Message,
            Visibility::Intelligence,
            EventStatus::Delivered,
        ),
    ] {
        let mut row = message(&public, "hiddenneedle");
        row.r#type = kind;
        row.visibility = visibility;
        row.status = status;
        hidden.push(append(&store, row).await);
    }
    let mut deleted = message(&public, "hiddenneedle");
    deleted
        .metadata
        .insert("deleted".into(), serde_json::json!(true));
    hidden.push(append(&store, deleted).await);
    let blind = room("unreadable-terminal");
    bind(&store, &blind, Access::ReadWrite).await;
    let mut terminal = store
        .list_bindings(&blind.id)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.channel_id == history.terminal)
        .unwrap();
    terminal.access = Access::WriteOnly;
    store.update_binding(terminal).await.unwrap();
    hidden.push(append(&store, message(&blind, "hiddenneedle")).await);
    let mut child = room("child");
    child
        .metadata
        .insert("parent_room_id".into(), serde_json::json!("caller"));
    let mut foreign = room("foreign");
    foreign.organization_id = Some("other".into());
    for (other, access) in [
        (&child, Access::ReadWrite),
        (&foreign, Access::ReadWrite),
        (&room("private"), Access::WriteOnly),
    ] {
        bind(&store, other, access).await;
        hidden.push(append(&store, message(other, "hiddenneedle")).await);
    }
    for citation in hidden {
        assert!(
            lookup(&history, &caller, None, Some(&citation))
                .await
                .is_err(),
            "{citation}"
        );
    }
    assert!(
        lookup(&history, &caller, Some("hiddenneedle"), None)
            .await
            .unwrap()
            .starts_with("No matching")
    );
    assert!(
        lookup(&history, &child, Some("hiddenneedle"), None)
            .await
            .is_err()
    );
    assert!(
        lookup(
            &history,
            &caller,
            None,
            Some("missing#18446744073709551615")
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn the_memory_tool_guides_a_missed_note_search_and_fits_a_cited_passage_in_its_budget() {
    use crate::project_memory::{QUOTA, View};
    use roomkit_ai::{Tool, ToolResult, memory::estimate_tokens};
    use roomkit_tools_memory::{FileMemory, MemoryTool};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::new(SqliteStoreOptions {
        path: dir.path().join("store"),
    });
    let (caller, old) = (room("follow-up"), room("prior"));
    bind(&store, &caller, Access::ReadWrite).await;
    bind(&store, &old, Access::ReadOnly).await;
    let hit = append(&store, message(&old, "A guide for the crew.")).await;
    let decision = append(
        &store,
        message(&old, &format!("Print it. {}", "Details. ".repeat(200))),
    )
    .await;
    let notes = MemoryTool::new(Arc::new(FileMemory::new(dir.path().join("notes"), QUOTA)));
    let tool = View::new(notes, Some(1024), None).tool(History::for_agent(Some(&store), "code"));
    let call =
        |args: serde_json::Value| tool.handler("memory".into(), args.as_object().unwrap().clone());
    let missed = call(serde_json::json!({"action":"search","query":"guide","reference":null}))
        .await
        .unwrap();
    assert!(matches!(missed, ToolResult::Text(text) if text.contains(r#""action":"history""#)));
    let turn = ToolTurnContextOptions {
        room: Some(caller.into()),
        ..Default::default()
    };
    let result = tool_turn_context(
        turn,
        call(serde_json::json!({"action":"read","reference":hit})),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(result, ToolResult::Text(s) if s.contains(&hit) && s.contains(&decision)
        && s.contains("Print it.") && estimate_tokens(&s) <= 32)
    );
}
