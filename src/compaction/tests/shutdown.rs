//! Compaction records finish before shutdown cancels unfinished summaries.

use super::*;
use roomkit::model::RoomId;
use roomkit::{
    ConversationStore, CreateRoomOptions, InMemoryLockManager, RoomKitOptions, RoomLockManager,
};

#[tokio::test]
async fn close_waits_for_the_timeline_commit_and_its_delivery_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let locks = Arc::new(InMemoryLockManager::default());
    let store = SqliteStore::new(roomkit_sqlite::SqliteStoreOptions {
        path: dir.path().join("history.sqlite3"),
    });
    let kit = RoomKit::new(RoomKitOptions {
        lock_manager: Some(locks.clone()),
        ..RoomKitOptions::with_store(store)
    });
    let room = RoomId::new("room").unwrap();
    kit.create_room(CreateRoomOptions {
        room_id: Some(room.clone()),
        ..Default::default()
    })
    .await
    .unwrap();
    let compactions = Arc::new(Compactions::new(&dir.path().join("compactions")));
    compactions.install(&kit, &ChannelId::new("terminal").unwrap());
    let held = locks.locked(&room).await.unwrap();
    let writing = compactions.clone();
    let record = tokio::spawn(async move {
        writing
            .record(("room", "code"), (compacted(7), 12), ((1, 1), TOOK, false))
            .await;
    });
    crate::sync::lock(&compactions.tasks).push(record);
    tokio::time::timeout(Duration::from_secs(1), async {
        while compactions.get("room", "code").is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut close = Box::pin(compactions.close());
    assert!(matches!(
        std::future::poll_fn(|cx| { std::task::Poll::Ready(close.as_mut().poll(cx)) }).await,
        std::task::Poll::Pending
    ));
    held.release().await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), close)
        .await
        .unwrap();
    let events = kit
        .store()
        .list_events(&room, Default::default())
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].metadata.contains_key("compaction"));
    let stored = kit.store().get_room(&room).await.unwrap().unwrap();
    assert_eq!(
        stored.delivered_index,
        i64::try_from(events[0].index).unwrap()
    );
    kit.close().await.unwrap();
}

/// Once the application closes, a compaction that waited for the turns
/// starts no more.
#[tokio::test]
async fn no_compaction_starts_once_closed() {
    let compactions = Arc::new(Compactions::new(Path::new("/nonexistent")));
    compactions.set_bounded("code", true);
    compactions.turn_admitted("room");
    assert_eq!(compactions.request("room", "code"), Requested::Queued);
    compactions.close().await;
    compactions.turn_finished("room");
    assert!(crate::sync::lock(&compactions.tasks).is_empty());
    assert!(!compactions.compacting("room"));
}

#[tokio::test]
async fn close_finishes_records_and_cancels_unfinished_summaries() {
    let dir = tempfile::tempdir().unwrap();
    let compactions = Arc::new(Compactions::new(dir.path()));
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let (finished, completed) = tokio::sync::oneshot::channel();
    let writing = compactions.clone();
    let record = tokio::spawn(async move {
        let _writing = writing.writing.read().await;
        started.send(()).unwrap();
        released.await.unwrap();
        finished.send(()).unwrap();
    });
    let summary = tokio::spawn(std::future::pending::<()>());
    let summary_abort = summary.abort_handle();
    crate::sync::lock(&compactions.tasks).extend([record, summary]);
    ready.await.unwrap();
    let closing = compactions.clone();
    let close = tokio::spawn(async move { closing.close().await });
    while !compactions
        .closed
        .load(std::sync::atomic::Ordering::Acquire)
    {
        tokio::task::yield_now().await;
    }
    assert!(!close.is_finished(), "shutdown cut a record in progress");
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), close)
        .await
        .unwrap()
        .unwrap();
    completed.await.unwrap();
    assert!(summary_abort.is_finished());
}

#[tokio::test]
async fn a_summary_finishing_after_close_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let compactions = Compactions::new(dir.path());
    compactions.close().await;
    compactions
        .record(("room", "code"), (compacted(7), 12), ((1, 1), TOOK, false))
        .await;
    assert_eq!(compactions.get("room", "code"), None);
    assert!(!compactions.path("room").exists());
    assert!(compactions.lines("room").is_none());
}
