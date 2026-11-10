#![expect(clippy::unwrap_used, reason = "test assertions")]

use super::*;
use roomkit_tools_plans::*;
use tokio_util::sync::CancellationToken;

fn scope(room: &str, agent: &str) -> PlanScope {
    PlanScope {
        room_id: room.into(),
        caller_id: agent.into(),
    }
}
fn input(title: &str) -> PlanCreateInput {
    PlanCreateInput {
        title: title.into(),
        objective: "Deliver a checked change".into(),
        steps: vec![PlanStep {
            step_id: "inspect".into(),
            description: "Inspect the project".into(),
            status: StepStatus::Pending,
        }],
    }
}
fn update(plan: &Plan, status: StepStatus) -> PlanUpdateInput {
    let mut steps = plan.steps.clone();
    steps[0].status = status;
    PlanUpdateInput {
        plan_id: plan.plan_id.clone(),
        expected_revision: plan.revision,
        title: None,
        objective: None,
        steps: Some(steps),
        step_changes: None,
    }
}

#[tokio::test]
async fn native_facades_preserve_plans_and_selection_across_restart_and_isolate_owners() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plans");
    let plans = Plans::new(path.clone());
    let owner = scope("session-a", "code");
    let cancel = CancellationToken::new();
    let tool = PlanCreateTool::new(std::sync::Arc::new(plans.clone()));
    let first = tool
        .call(&owner, input("First"), &cancel)
        .await
        .unwrap()
        .data;
    let second = tool
        .call(&owner, input("Second"), &cancel)
        .await
        .unwrap()
        .data;
    assert_eq!(
        plans.snapshot(&owner).await.unwrap().selected,
        Some(first.plan_id.clone())
    );
    plans.select(&owner, &second.plan_id).await.unwrap();
    assert!(plans.close().await);
    assert!(
        plans
            .list(&owner, &PlanListInput {}, &cancel)
            .await
            .is_err()
    );
    let plans = Plans::new(path);
    let doc = plans.snapshot(&owner).await.unwrap();
    assert_eq!(doc.selected.as_ref(), Some(&second.plan_id));
    assert_eq!(doc.plans.len(), 2);
    for other in [scope("session-b", "code"), scope("session-a", "reviewer")] {
        assert!(
            plans
                .list(&other, &PlanListInput {}, &cancel)
                .await
                .unwrap()
                .plans
                .is_empty()
        );
        assert!(
            plans
                .read(
                    &other,
                    &PlanReadInput {
                        plan_id: first.plan_id.clone()
                    },
                    &cancel
                )
                .await
                .is_err()
        );
        assert!(plans.select(&other, &first.plan_id).await.is_err());
    }
    let other_project = Plans::new(dir.path().join("other-project/plans"));
    assert!(
        other_project
            .list(&owner, &PlanListInput {}, &cancel)
            .await
            .unwrap()
            .plans
            .is_empty()
    );
    plans
        .delete(
            &owner,
            &PlanDeleteInput {
                plan_id: second.plan_id,
                expected_revision: 1,
            },
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        plans.snapshot(&owner).await.unwrap().selected,
        None,
        "Do not silently choose a different plan"
    );
    assert_eq!(
        plans
            .list(&owner, &PlanListInput {}, &cancel)
            .await
            .unwrap()
            .plans
            .len(),
        1
    );
    plans
        .create(&owner, &input("Third"), &cancel)
        .await
        .unwrap();
    assert_eq!(
        plans.snapshot(&owner).await.unwrap().selected,
        None,
        "Creating another plan must not override an explicit need to choose"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.path().join("plans"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for file in std::fs::read_dir(dir.path().join("plans")).unwrap() {
            assert_eq!(
                file.unwrap().metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}

#[tokio::test]
async fn concurrent_revisions_cancelled_calls_and_capacity_never_lose_data() {
    let dir = tempfile::tempdir().unwrap();
    let plans = Plans::new(dir.path().join("plans"));
    let owner = scope("session", "code");
    let cancel = CancellationToken::new();
    let mut changes = plans.subscribe();
    let plan = plans.create(&owner, &input("Work"), &cancel).await.unwrap();
    changes.borrow_and_update();
    let no_op = plans
        .update(&owner, &update(&plan, StepStatus::Pending), &cancel)
        .await
        .unwrap();
    assert_eq!(no_op.revision, 1);
    assert!(!changes.has_changed().unwrap());
    let a = update(&plan, StepStatus::InProgress);
    let b = update(&plan, StepStatus::Blocked);
    let (a, b) = tokio::join!(
        plans.update(&owner, &a, &cancel),
        plans.update(&owner, &b, &cancel)
    );
    assert_ne!(
        a.is_ok(),
        b.is_ok(),
        "Exactly one concurrent revision may commit"
    );
    assert!(changes.has_changed().unwrap());
    let saved = plans
        .read(
            &owner,
            &PlanReadInput {
                plan_id: plan.plan_id.clone(),
            },
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(saved.revision, 2);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        plans
            .update(&owner, &update(&saved, StepStatus::Completed), &cancelled)
            .await
            .is_err()
    );
    assert!(
        plans
            .delete(
                &owner,
                &PlanDeleteInput {
                    plan_id: plan.plan_id.clone(),
                    expected_revision: 1
                },
                &cancel
            )
            .await
            .is_err()
    );
    for _ in 1..64 {
        plans
            .create(&owner, &input("Retained"), &cancel)
            .await
            .unwrap();
    }
    assert!(
        plans
            .create(&owner, &input("Over capacity"), &cancel)
            .await
            .is_err()
    );
    assert_eq!(
        plans
            .list(&owner, &PlanListInput {}, &cancel)
            .await
            .unwrap()
            .plans
            .len(),
        64
    );
    assert_eq!(
        plans
            .read(
                &owner,
                &PlanReadInput {
                    plan_id: plan.plan_id
                },
                &cancel
            )
            .await
            .unwrap(),
        saved
    );
}

#[tokio::test]
async fn corrupt_or_redirected_storage_is_reported_without_replacing_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plans");
    let plans = Plans::new(path.clone());
    let owner = scope("session", "code");
    let cancel = CancellationToken::new();
    plans.create(&owner, &input("Work"), &cancel).await.unwrap();
    let file = std::fs::read_dir(&path)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(&file, b"broken PRIVATE plan").unwrap();
    let error = plans
        .create(&owner, &input("Other"), &cancel)
        .await
        .unwrap_err()
        .to_string();
    assert!(!error.contains("PRIVATE"));
    assert_eq!(std::fs::read(&file).unwrap(), b"broken PRIVATE plan");
    #[cfg(unix)]
    {
        let linked = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &linked).unwrap();
        let linked = Plans::new(linked);
        assert!(
            linked
                .create(&scope("different", "code"), &input("Other"), &cancel)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_dir(path).unwrap().count(), 1);
    }
}
