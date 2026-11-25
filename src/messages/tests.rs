//! Check admission and replay against the same RoomKit store as two terminals.
#![expect(clippy::unwrap_used, reason = "fixture failures must fail the test")]
use super::{record::Mail, reply};
use roomkit::model::EventId;
use roomkit::{ConversationStore, RoomKit, RoomKitOptions};
use roomkit_sqlite::{SqliteStore, SqliteStoreOptions};

#[tokio::test]
async fn replay_validates_the_content_and_reply_ids_stay_in_the_recipient_room() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::new(SqliteStoreOptions {
        path: dir.path().join("history.sqlite3"),
    });
    let kit = RoomKit::new(RoomKitOptions::with_store(store));
    let sender = kit.create_room(Default::default()).await.unwrap().id;
    let recipient = kit.create_room(Default::default()).await.unwrap().id;
    let message = Mail {
        from: sender.clone(),
        text: "Which branch?".into(),
        remote: None,
        kind: super::record::Kind::Message,
    };
    let id = EventId::new("session-msg-example").unwrap();
    let save = || message.write(&kit, &recipient, id.clone(), None);
    let (one, two) = tokio::join!(save(), save());
    one.unwrap();
    two.unwrap();
    let conflict = Mail {
        text: "A different question".into(),
        ..message.clone()
    };
    assert!(
        conflict
            .write(&kit, &recipient, id.clone(), None)
            .await
            .is_err()
    );
    assert!(
        reply::original(&kit, &sender, id.to_string())
            .await
            .is_err()
    );
    let (event, original) = reply::original(&kit, &recipient, id.to_string())
        .await
        .unwrap();
    assert_eq!(original.text, "Which branch?");
    assert_eq!(event.index, 0);
    assert_eq!(
        kit.store()
            .get_event_count(&recipient, Default::default())
            .await
            .unwrap(),
        1
    );
    kit.close().await.unwrap();
}

#[test]
fn only_terminal_correspondence_uses_italics() {
    use roomkit_tui::protocol::Message;
    let styled = |actor, id| {
        super::emphasis(&Message::new(id, actor, "Reply from @review"))
            .is_some_and(|style| style.italic)
    };
    for id in [
        "session-msg-a",
        "sent-session-msg-a",
        "session-reply-session-msg-a",
    ] {
        assert!(styled("terminal", id));
        assert!(!styled("agent", id));
    }
    assert!(!styled("terminal", "session-notice-session-msg-a"));
    assert!(!styled("terminal", "unrelated"));
}

#[tokio::test]
async fn concurrent_network_outcomes_cannot_both_enter_the_asking_room() -> crate::Result<()> {
    use super::record::{Kind, Route};
    use roomkit::model::ChannelId;
    let dir = tempfile::tempdir()?;
    let kit = RoomKit::new(RoomKitOptions::with_store(SqliteStore::new(
        SqliteStoreOptions {
            path: dir.path().join("history.sqlite3"),
        },
    )));
    let room = kit.create_room(Default::default()).await?.id;
    let original = EventId::new("session-msg-question")?;
    let reply = Mail {
        from: roomkit::model::RoomId::new("review")?,
        text: "Reviewed".into(),
        kind: Kind::Reply(original.clone()),
        remote: Some(Route {
            from: ChannelId::new("sender")?,
            to: ChannelId::new("receiver")?,
        }),
    };
    let notice = Mail {
        kind: Kind::Notice(original),
        text: "No answer".into(),
        ..reply.clone()
    };
    let (a, b) = tokio::join!(
        reply.write(&kit, &room, super::reply::response_id(&reply.kind)?, None),
        notice.write(&kit, &room, super::reply::response_id(&notice.kind)?, None)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(
        kit.store()
            .get_event_count(&room, Default::default())
            .await?,
        1
    );
    kit.close().await?;
    Ok(())
}

#[tokio::test]
async fn an_unreadable_store_is_not_an_undesignated_agent() -> crate::Result<()> {
    let dir = tempfile::tempdir()?;
    let kit = RoomKit::new(RoomKitOptions::with_store(SqliteStore::new(
        SqliteStoreOptions {
            path: dir.path().join("history.sqlite3"),
        },
    )));
    let room = kit.create_room(Default::default()).await?.id;
    assert!(crate::sessions::designated(&kit, &room).await?.is_none());
    kit.close().await?;
    assert!(crate::sessions::designated(&kit, &room).await.is_err());
    Ok(())
}

#[tokio::test]
async fn delegated_sessions_are_scoped_to_the_parent_and_reply_address() -> crate::Result<()> {
    use super::record::Kind;
    use roomkit::model::{ChannelId, RoomId};
    use serde_json::json;
    let dir = tempfile::tempdir()?;
    let kit = RoomKit::new(RoomKitOptions::with_store(SqliteStore::new(
        SqliteStoreOptions {
            path: dir.path().join("history.sqlite3"),
        },
    )));
    let parent = kit.create_room(Default::default()).await?;
    let foreign = kit.create_room(Default::default()).await?;
    let metadata = [
        ("parent_room_id".into(), json!(parent.id)),
        ("task_agent_id".into(), json!(crate::remote::AGENT)),
        ("task_input".into(), json!("Review")),
    ]
    .into_iter()
    .collect();
    let child = kit
        .create_room(roomkit::CreateRoomOptions {
            metadata,
            ..Default::default()
        })
        .await?;
    let other = RoomId::new("unrelated")?;
    assert_eq!(
        super::delegated::contacts(&kit, &parent).await?[0]["to"],
        json!(child.id)
    );
    assert!(super::delegated::contacts(&kit, &foreign).await?.is_empty());
    assert!(
        super::delegated::destination(&kit, &foreign, &child.id)
            .await?
            .is_none()
    );
    let question = Mail {
        from: parent.id.clone(),
        remote: None,
        text: "Which branch?".into(),
        kind: Kind::ToSession {
            to: child.id.clone(),
            agent: ChannelId::new("code")?,
            turn: "asking-turn".into(),
        },
    };
    let id = EventId::new("session-msg-q")?;
    question.write(&kit, &parent.id, id.clone(), None).await?;
    assert!(
        reply::original(&kit, &foreign.id, id.to_string())
            .await
            .is_err()
    );
    let (row, _) = reply::original(&kit, &child.id, id.to_string()).await?;
    let mut answer = Mail {
        from: other,
        remote: None,
        text: "branch-a".into(),
        kind: Kind::Reply(id.clone()),
    };
    assert!(
        super::request::local(&kit, &row, &answer, &id)
            .await?
            .is_none()
    );
    answer.from = child.id;
    let request = super::request::local(&kit, &row, &answer, &id)
        .await?
        .unwrap();
    assert_eq!(request.turn, "asking-turn");
    assert_eq!(request.question, "Which branch?");
    kit.close().await?;
    Ok(())
}
