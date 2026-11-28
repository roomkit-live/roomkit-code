use super::*;
use roomkit::{RoomKit, RoomKitOptions, model::ChannelId};
use roomkit_providers_sqlite::{SqliteStore, SqliteStoreOptions};
#[tokio::test]
async fn restart_closes_an_interrupted_question_once_and_leaves_new_mail_pending()
-> crate::Result<()> {
    use super::super::{dispatch::Checkpoint, record::Kind};
    use roomkit::model::EventId;
    let dir = tempfile::tempdir()?;
    let kit = RoomKit::new(RoomKitOptions::with_store(SqliteStore::new(
        SqliteStoreOptions {
            path: dir.path().join("history.sqlite3"),
        },
    )));
    let parent = kit.create_room(Default::default()).await?.id;
    let child = kit.create_room(Default::default()).await?.id;
    let terminal = ChannelId::new(crate::names::TERMINAL)?;
    kit.register_channel(roomkit_tui_engine::ConversationChannel::new(
        terminal.clone(),
    ))?;
    kit.attach_channel(&parent, &terminal, Default::default())
        .await?;
    let question = Mail {
        from: parent.clone(),
        remote: None,
        text: "Which revision?".into(),
        kind: Kind::ToSession {
            to: child,
            agent: ChannelId::new("code")?,
            turn: "asking-turn".into(),
        },
    };
    let first = EventId::new("session-msg-admitted")?;
    let next = EventId::new("session-msg-fresh")?;
    question.write(&kit, &parent, first.clone(), None).await?;
    question.write(&kit, &parent, next.clone(), None).await?;
    let row = kit
        .store()
        .get_event(&first)
        .await?
        .ok_or("Missing question")?;
    Checkpoint::Steering.save(&kit, &row).await?;
    recover_session_questions(&kit, &parent).await?;
    recover_session_questions(&kit, &parent).await?;
    assert!(answered(&kit, &row).await?);
    let next_row = kit
        .store()
        .get_event(&next)
        .await?
        .ok_or("Missing pending question")?;
    assert!(!answered(&kit, &next_row).await?);
    let notice = super::super::reply::response_id(&Kind::Notice(first))?;
    assert!(kit.store().get_event(&notice).await?.is_some());
    kit.close().await?;
    Ok(())
}

#[tokio::test]
async fn an_idle_inbox_advances_past_unrelated_history_and_backs_off() -> crate::Result<()> {
    let dir = tempfile::tempdir()?;
    let kit = RoomKit::new(RoomKitOptions::with_store(SqliteStore::new(
        SqliteStoreOptions {
            path: dir.path().join("history.sqlite3"),
        },
    )));
    let room = kit.create_room(Default::default()).await?.id;
    let row = crate::system_row::SystemRow {
        room: room.as_str(),
        id: "ordinary",
        text: "Other work",
        about: ("source", json!("test")),
    }
    .store(&kit, &ChannelId::new("terminal")?)
    .await?;
    let messages = Messages::default();
    let diagnostics =
        crate::diagnostics::Diagnostics::open(dir.path().join("diagnostics.jsonl"), false)?;
    let mut cursor = Cursor::default();
    let mut posts = Posts::new();
    assert!(
        !messages
            .drain(&kit, &room, &mut cursor, &mut posts, &diagnostics)
            .await?
    );
    assert_eq!(cursor.after(), Some(row.index));
    assert_eq!(
        Cursor::load(&kit, &room, false).await?.after(),
        cursor.after()
    );
    assert_eq!(
        idle_delay(Duration::from_secs(4), false),
        Duration::from_secs(5)
    );
    assert_eq!(
        idle_delay(Duration::from_secs(5), true),
        Duration::from_millis(750)
    );
    kit.close().await?;
    Ok(())
}

#[tokio::test]
async fn offline_local_mail_passes_a_pending_remote_delivery_across_restarts() -> crate::Result<()>
{
    use super::super::{
        dispatch::Checkpoint,
        record::{Kind, Route},
    };
    use roomkit::model::EventId;
    let dir = tempfile::tempdir()?;
    let kit = RoomKit::new(RoomKitOptions::with_store(SqliteStore::new(
        SqliteStoreOptions {
            path: dir.path().join("history.sqlite3"),
        },
    )));
    let owner = kit.create_room(Default::default()).await?.id;
    let sender = kit.create_room(Default::default()).await?.id;
    let terminal = ChannelId::new(crate::names::TERMINAL)?;
    kit.register_channel(roomkit_tui_engine::ConversationChannel::new(
        terminal.clone(),
    ))?;
    for room in [&sender, &owner] {
        kit.attach_channel(room, &terminal, Default::default())
            .await?;
    }
    let local = Mail {
        from: sender,
        remote: None,
        kind: Kind::Message,
        text: "Local question".into(),
    };
    let remote = Mail {
        remote: Some(Route {
            from: ChannelId::new("peer-sender")?,
            to: ChannelId::new("peer-owner")?,
        }),
        ..local.clone()
    };
    let remote_id = EventId::new("session-msg-remote")?;
    let local_id = EventId::new("session-msg-local")?;
    remote.write(&kit, &owner, remote_id.clone(), None).await?;
    local.write(&kit, &owner, local_id.clone(), None).await?;
    let remote_event = kit
        .store()
        .get_event(&remote_id)
        .await?
        .ok_or("Missing remote record")?;
    let local_event = kit
        .store()
        .get_event(&local_id)
        .await?
        .ok_or("Missing local record")?;
    // Older versions could have stopped after claiming the remote handover.
    let legacy = [
        ("id".into(), json!(remote_id)),
        ("dispatch".into(), json!("Steering")),
    ];
    kit.store()
        .set_private_state(
            &owner,
            "roomkit_code:session_mail_dispatch:v1",
            Some(legacy.into_iter().collect()),
        )
        .await?;
    let diag = crate::diagnostics::Diagnostics::open(dir.path().join("log"), false)?;
    let mut cursor = Cursor::load(&kit, &owner, true).await?;
    Messages::default()
        .drain(&kit, &owner, &mut cursor, &mut Posts::new(), &diag)
        .await?;
    // No agent is attached. The local sender gets its delivery notice immediately.
    let notice = super::super::reply::response_id(&Kind::Notice(local_id))?;
    assert!(kit.store().get_event(&notice).await?.is_some());
    assert!(Cursor::load(&kit, &owner, true).await?.after() >= Some(local_event.index));
    let rejoined = Cursor::load(&kit, &owner, false).await?;
    assert!(rejoined.after() < Some(remote_event.index));
    assert!(rejoined.pending(true, remote_event.index));
    assert!(!rejoined.pending(false, local_event.index));
    Checkpoint::Delivered.save(&kit, &local_event).await?;
    assert!(matches!(
        Checkpoint::read(&kit, &remote_event).await?,
        Some(Checkpoint::Steering)
    ));
    kit.close().await?;
    Ok(())
}
