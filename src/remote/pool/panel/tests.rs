//! The terminal bounds each picker and panel. Keep every machine and profile reachable.
use super::*;
use crate::remote::pool::{Worker, WorkerProfile};

fn worker(name: String) -> Row {
    Row {
        worker: Worker {
            name,
            instance: "instance".into(),
            os: "macos".into(),
            arch: "aarch64".into(),
            host_os: None,
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
