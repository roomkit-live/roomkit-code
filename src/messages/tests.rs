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
