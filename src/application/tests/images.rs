use super::*;
use roomkit_tui::{Client, protocol::OperationId};

async fn next(client: &Client) -> Event {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match client.try_recv() {
                Ok(event) => return event,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(2)).await
                }
                Err(error) => panic!("Attachment disconnected: {error}"),
            }
        }
    })
    .await
    .unwrap()
}
async fn attach(
    app: &Application,
    workspace: &std::path::Path,
) -> (
    Client,
    tokio::task::JoinHandle<std::result::Result<(), roomkit_cli_roomkit::AdapterError>>,
) {
    let (client, host) = connection(NonZeroUsize::new(64).unwrap());
    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: workspace.to_path_buf(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
    };
    let task = tokio::spawn(app.session().run_with_commands(host, handler));
    assert!(matches!(next(&client).await, Event::Snapshot(_)));
    (client, task)
}
fn request(app: &Application, command: Command) -> Request {
    Request {
        id: RequestId(91),
        conversation: app.view.room.as_str().into(),
        operation: OperationId(uuid::Uuid::new_v4().to_string()),
        command,
    }
}
async fn completed(client: &Client) -> Vec<Event> {
    let mut events = vec![];
    loop {
        let event = next(client).await;
        assert!(
            !matches!(&event, Event::Reply { error: Some(_), .. }),
            "{event:?}"
        );
        if let Event::OperationFinished { outcome, .. } = event {
            assert_eq!(outcome, RunOutcome::Completed);
            return events;
        }
        events.push(event);
    }
}
#[tokio::test]
async fn images_reach_the_provider_and_resume_from_private_references() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    assert!(!app.view.can_attach_images);
    edit_profile(&mut app, "agents", "edit:code", &[("vision", "enabled")]).await;
    assert!(app.view.can_attach_images);
    let image = crate::images::tests::fixture().unwrap();
    let (client, task) = attach(&app, root.path()).await;
    client
        .try_send(request(
            &app,
            Command::SubmitImages {
                agent: "code".into(),
                message: Box::new(roomkit_tui::protocol::ImageMessage {
                    text: "Describe this image.".into(),
                    images: vec![image.clone()],
                }),
            },
        ))
        .unwrap();
    let events = completed(&client).await;
    let payload = format!("data:image/png;base64,{}", image.data);
    assert!(events.iter().any(|event| matches!(event, Event::Item { item: Item::Message(m), .. } if m.text.contains("Image 1"))));
    assert!(!format!("{events:?}").contains(&image.data));
    let first = mock.requests.lock().unwrap().first().unwrap().clone();
    assert!(
        first["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["content"]
                .as_array()
                .is_some_and(|parts| parts.iter().any(|p| p["image_url"]["url"] == payload))),
        "{first}"
    );
    let timeline = app
        .kit
        .get_timeline(&app.view.room, Default::default())
        .await
        .unwrap();
    let stored = serde_json::to_string(&timeline).unwrap();
    assert!(stored.contains("attachment://"));
    assert!(!stored.contains(&image.data));
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
    drop(app);
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert!(app.view.can_attach_images);
    let (client, task) = attach(&app, root.path()).await;
    client
        .try_send(request(
            &app,
            Command::Submit {
                agent: "code".into(),
                text: "What color is it?".into(),
            },
        ))
        .unwrap();
    completed(&client).await;
    let resumed = mock.requests.lock().unwrap().last().unwrap().clone();
    assert!(resumed["messages"].as_array().unwrap().iter().any(|m| {
        m["content"]
            .as_array()
            .is_some_and(|parts| parts.iter().any(|p| p["image_url"]["url"] == payload))
    }));
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
}
#[tokio::test]
async fn invalid_images_and_text_only_agents_never_call_the_provider() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(vec![]).await;
    let settings = settings(root.path(), state.path(), &mock.base_url);
    let mut app = Application::open(&settings, String::new()).await.unwrap();
    for vision in [false, true] {
        if vision {
            edit_profile(&mut app, "agents", "edit:code", &[("vision", "enabled")]).await;
        }
        let (client, task) = attach(&app, root.path()).await;
        let mut image = crate::images::tests::fixture().unwrap();
        if vision {
            image.width = 3;
        }
        client
            .try_send(request(
                &app,
                Command::SubmitImages {
                    agent: "code".into(),
                    message: Box::new(roomkit_tui::protocol::ImageMessage {
                        text: "".into(),
                        images: vec![image],
                    }),
                },
            ))
            .unwrap();
        loop {
            if let Event::Reply { error, .. } = next(&client).await {
                assert!(error.is_some());
                break;
            }
        }
        assert!(mock.requests.lock().unwrap().is_empty());
        assert!(!app._storage.database.with_file_name("attachments").exists());
        drop(client);
        task.await.unwrap().unwrap();
    }
    app.close().await.unwrap();
}
