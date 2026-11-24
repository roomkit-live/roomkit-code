//! The terminal bounds each picker and panel. Keep every machine and profile reachable.
use super::*;
use crate::remote::pool::{Worker, WorkerProfile};

fn worker(name: String) -> Row {
    Row {
        serving: true,
        worker: Worker {
            name,
            instance: "instance".into(),
            os: "macos".into(),
            arch: "aarch64".into(),
            host_os: None,
            workspace: None,
            checkout: None,
            git_repository: None,
            commit_tests: false,
            version: None,
            model: None,
            profiles: vec![],
            labels: Default::default(),
            slots: 1,
            metrics: Default::default(),
        },
        age: 0,
        used: 0,
    }
}

#[test]
fn a_large_directory_keeps_navigation_and_the_last_machine() -> Result<(), String> {
    for page in [0, 1] {
        let rows = (0..510).map(|i| worker(format!("worker-{i:03}"))).collect();
        let CommandResult::Events(events) = directory("room", "pool", rows, page) else {
            return Err("missing directory".into());
        };
        let [Event::Choices { choices, .. }] = events.as_slice() else {
            return Err("missing picker".into());
        };
        let has = |value: &str| choices.iter().any(|c| c.value == value);
        assert!(choices.len() <= 500 && has("configure") && has("tasks"));
        assert!(if page == 0 {
            has("page:1") && has("worker:worker-000")
        } else {
            has("page:0") && has("worker:worker-509")
        });
    }
    Ok(())
}

#[test]
fn a_large_worker_offers_all_its_profiles_in_the_picker() -> Result<(), String> {
    let mut row = worker("laptop".into());
    row.worker.profiles = (0..70)
        .map(|i| WorkerProfile {
            id: format!("agent-{i}"),
            label: format!("Agent {i}"),
            model: format!("model-{i}"),
            tools: vec![],
        })
        .collect();
    let rows = [row];
    let CommandResult::Events(events) = worker_details("room", &rows, "laptop") else {
        return Err("missing worker".into());
    };
    let [Event::Panel { panel, .. }] = events.as_slice() else {
        return Err("missing panel".into());
    };
    assert!(
        panel.actions.iter().any(|a| a.value == "profiles:laptop") && panel.actions.len() <= 64
    );
    let CommandResult::Events(events) = profile_picker("room", &rows, "laptop") else {
        return Err("missing profiles".into());
    };
    let [Event::Choices { choices, .. }] = events.as_slice() else {
        return Err("missing picker".into());
    };
    assert!(
        choices
            .iter()
            .any(|c| c.value == "profile:laptop/agent-69" && c.description == "model-69")
    );
    Ok(())
}

#[test]
fn a_legacy_advertisement_keeps_its_unknown_workspace_and_version() -> crate::Result<()> {
    let mut row = worker("laptop".into());
    let mut legacy = serde_json::to_value(&row.worker)?;
    let object = legacy.as_object_mut().ok_or("missing worker fields")?;
    object.remove("workspace");
    object.remove("version");
    object.remove("checkout");
    object.remove("git_repository");
    object.remove("commit_tests");
    row.worker = serde_json::from_value(legacy)?;
    assert!(row.worker.checkout.is_none() && !row.worker.commit_tests);
    let details = resource_lines(&row);
    assert!(
        details
            .iter()
            .any(|l| l.starts_with("Workspace") && l.ends_with("Not reported"))
    );
    assert!(
        details
            .iter()
            .any(|l| l.starts_with("Version") && l.ends_with("Not reported"))
    );
    assert_eq!(worker_choice(&row).label, "laptop · Online");
    row.worker.workspace = Some("/tmp/project\ncopy".into());
    assert_eq!(row.worker.workspace_display(), "/tmp/project\\ncopy");
    Ok(())
}

#[test]
fn commit_placement_requires_support_on_the_selected_profile() -> crate::Result<()> {
    use crate::remote::pool::{CARGO_TEST, Requirements};
    use serde_json::json;
    let mut w = worker("machine".into()).worker;
    let sha = "a".repeat(40);
    let context = json!({"executor": {"test_commit": sha}});
    let req = Requirements::parse(Some(&context))?;
    w.profiles.push(WorkerProfile {
        id: "code".into(),
        label: "Code".into(),
        model: "model".into(),
        tools: vec![CARGO_TEST.into()],
    });
    assert!(!req.matches(&w));
    w.commit_tests = true; // Older workers could advertise this outside a repository.
    assert!(!req.matches(&w));
    w.git_repository = Some("/project".into()); // Status is still being sampled.
    assert!(req.matches(&w));
    w.git_repository = None;
    w.checkout = Some(serde_json::from_value(
        json!({"repository":"/project","branch":"feature","head":sha,"dirty":true}),
    )?);
    assert!(req.matches(&w));
    w.profiles[0].tools.clear();
    assert!(!req.matches(&w));
    assert!(req.check_call(CARGO_TEST, &Default::default()).is_err());
    let args = [("commit".into(), sha.into())].into_iter().collect();
    assert!(req.check_call(CARGO_TEST, &args).is_ok());
    assert!(req.check_call("edit", &args).is_err());
    assert!(Requirements::parse(Some(&json!({"executor":{"test_commit":"main"}}))).is_err());
    Ok(())
}

#[test]
fn consultation_needs_an_opted_in_task_and_a_profile_that_can_reply() -> crate::Result<()> {
    use crate::remote::pool::{ANSWER_PEER, ASK_PEER, Requirements};
    let context = serde_json::json!({"executor":{"consultation":true}});
    assert!(Requirements::for_remote_task(Some(&context), false).is_err());
    let req = Requirements::for_remote_task(Some(&context), true)?;
    assert!(
        Requirements::default()
            .check_call(ASK_PEER, &Default::default())
            .is_err()
    );
    assert!(req.check_call(ANSWER_PEER, &Default::default()).is_ok());
    let mut w = worker("worker".into()).worker;
    assert!(!req.matches(&w));
    w.profiles.push(WorkerProfile {
        id: "reviewer".into(),
        label: "Reviewer".into(),
        model: "model".into(),
        tools: vec![ASK_PEER.into()],
    });
    assert!(!req.matches(&w));
    w.profiles[0].tools.push(ANSWER_PEER.into());
    assert!(req.matches(&w));
    Ok(())
}
