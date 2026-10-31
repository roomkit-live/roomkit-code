//! The sidebar lists what this conversation's agents changed, including shell
//! effects, and the list returns with the conversation.
use super::*;
use roomkit_tui::protocol::{SidebarContent, SidebarSection};
use roomkit_tui_engine::{CommandHandler, CommandResult};

async fn files(app: &Application) -> SidebarSection {
    let (sidebar, _) = app.profiles.sidebar(&app.view).await;
    sidebar
        .sections
        .into_iter()
        .find(|section| section.title == "Files")
        .unwrap()
}

fn rows(section: &SidebarSection) -> Vec<(String, String)> {
    let SidebarContent::Entries { entries } = &section.content else {
        panic!("Expected file entries");
    };
    entries
        .iter()
        .map(|entry| (entry.label.clone(), entry.detail.clone()))
        .collect()
}

#[tokio::test]
async fn sidebar_lists_tool_and_shell_changes_and_they_return_with_the_session() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("old.txt"), "before").unwrap();
    std::fs::write(root.path().join("keep.txt"), "kept").unwrap();
    std::fs::write(root.path().join("notes.md"), "# Notes").unwrap();
    let mock = Mock::start(vec![
        ("write", json!({"path":"new.txt","content":"new"})),
        (
            "edit",
            json!({"path":"old.txt","old_text":"before","new_text":"after"}),
        ),
        ("read", json!({"path":"notes.md"})),
        (
            "shell",
            json!({"command":"printf made > made.txt; rm keep.txt"}),
        ),
    ])
    .await;
    let mut settings = settings(root.path(), state.path(), &mock.base_url);
    settings.mode = crate::permissions::Mode::Yolo;
    let app = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(
        rows(&files(&app).await),
        [("No files changed".into(), String::new())]
    );
    let session = turn(&app).await;
    let failed: Vec<_> = session
        .snapshot()
        .unwrap()
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Tool { call, .. } if call.state != ToolState::Succeeded => Some(&call.name),
            _ => None,
        })
        .collect();
    assert!(failed.is_empty(), "{failed:?}");
    let expected: [(String, String); 5] = [
        ("made.txt".into(), "Created · shell".into()),
        ("keep.txt".into(), "Deleted · shell".into()),
        ("old.txt".into(), "Modified".into()),
        ("new.txt".into(), "Created".into()),
        ("1 file read".into(), String::new()),
    ];
    assert_eq!(rows(&files(&app).await), expected);

    // /files lists everything, including the files only read.
    let handler = crate::sessions::Sessions {
        kit: app.kit.clone(),
        workspace: settings.workspace.clone(),
        channel: app.channel.id().clone(),
        policy: app.policy.clone(),
        profiles: app.profiles.clone(),
        services: app.services.clone(),
    };
    let request = Request::new(
        RequestId::new(1),
        "files",
        app.view.room.as_str(),
        Command::Invoke {
            name: "files".into(),
            arguments: String::new(),
        },
    );
    let Ok(CommandResult::Events(events)) = handler.handle(request, app.view.clone()).await else {
        panic!("Expected the files panel");
    };
    let Some(Event::Panel { panel, .. }) = events.first() else {
        panic!("Expected a panel: {events:?}");
    };
    assert!(panel.body.contains("Changed (4)") && panel.body.contains("Read only (1)"));
    assert!(panel.body.contains("  notes.md"), "{}", panel.body);
    app.close().await.unwrap();
    drop(app);

    // The list is stored with the conversation and returns when it is resumed.
    let resumed = Application::open(&settings, String::new()).await.unwrap();
    assert_eq!(rows(&files(&resumed).await), expected);
    resumed.close().await.unwrap();
}
