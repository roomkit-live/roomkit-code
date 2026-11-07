//! The launch's decisions on scripted host events, without an engine.

#![expect(clippy::unwrap_used, reason = "fixture failures must fail the test")]
#![expect(clippy::panic, reason = "an unexpected step must fail the test")]

use roomkit_tui::protocol::{Actor, ErrorCode, Message, Permission, Rejection};

use super::*;

/// The operation this launch's message goes under.
const MESSAGE: &str = "launch-message";

fn driver() -> Driver {
    Driver::new("Fix the build.".into(), "launch".into())
}

fn snapshot() -> Event {
    Event::Snapshot(Snapshot::new("room", "Title", Actor::new("agent", "Agent")))
}

fn started(run: &str) -> Event {
    Event::RunStarted {
        conversation: "room".into(),
        run: run.into(),
        answers: None,
    }
}

fn finished(run: &str, outcome: RunOutcome) -> Event {
    Event::RunFinished {
        conversation: "room".into(),
        run: run.into(),
        outcome,
    }
}

fn operation(outcome: RunOutcome) -> Event {
    Event::OperationFinished {
        conversation: "room".into(),
        operation: MESSAGE.into(),
        outcome,
    }
}

fn message(actor: &str, text: &str) -> Event {
    Event::MessageFinalized {
        conversation: "room".into(),
        draft: "draft".into(),
        message: Message::new("m", actor, text),
    }
}

fn sent(steps: &[Step]) -> Vec<&Command> {
    steps
        .iter()
        .filter_map(|step| match step {
            Step::Send(request) => Some(&request.command),
            Step::Tell(_) => None,
        })
        .collect()
}

/// Drive a launch through its message's whole turn.
fn turn(driver: &mut Driver, outcome: RunOutcome) {
    driver.on_event(&snapshot());
    driver.on_event(&started("run"));
    driver.on_event(&message("agent", "Done."));
    driver.on_event(&finished("run", outcome));
    driver.on_event(&operation(outcome));
}

#[test]
fn the_first_snapshot_sends_the_message_to_its_selected_agent_once() {
    let mut driver = driver();
    let steps = driver.on_event(&snapshot());
    let [Step::Send(request)] = steps.as_slice() else {
        panic!("one request expected: {steps:?}");
    };
    assert_eq!(request.id, RequestId::new(1));
    assert_eq!(request.operation.as_str(), MESSAGE);
    assert_eq!(request.conversation.as_str(), "room");
    assert_eq!(
        request.command,
        Command::Submit {
            agent: "agent".into(),
            text: "Fix the build.".into()
        }
    );
    // A later snapshot, a refreshed view, never sends it again.
    assert!(driver.on_event(&snapshot()).is_empty());
    assert_eq!(driver.ending(), None);
}

#[test]
fn a_completed_turn_ends_the_launch_with_the_agent_s_last_message() {
    let mut driver = driver();
    driver.on_event(&snapshot());
    driver.on_event(&started("run"));
    driver.on_event(&message("agent", "Looking."));
    driver.on_event(&message("local-user", "Not the agent."));
    driver.on_event(&message("agent", "Fixed."));
    driver.on_event(&finished("run", RunOutcome::Completed));
    // The operation still settles: the launch waits for it.
    assert_eq!(driver.ending(), None);
    driver.on_event(&operation(RunOutcome::Completed));
    assert_eq!(driver.ending(), Some(Ending::Completed));
    assert_eq!(driver.answer(), Some("Fixed."));
    let (conversation, agent) = driver.conversation().unwrap();
    assert_eq!((conversation.as_str(), agent.as_str()), ("room", "agent"));
}

#[test]
fn a_turn_cut_by_its_round_limit_fails_the_launch() {
    let mut driver = driver();
    turn(&mut driver, RunOutcome::Failed);
    assert_eq!(driver.ending(), Some(Ending::Failed));
    // The run alone failing fails the launch, even if the operation completed.
    let mut alone = self::driver();
    alone.on_event(&snapshot());
    alone.on_event(&started("run"));
    alone.on_event(&finished("run", RunOutcome::Failed));
    alone.on_event(&operation(RunOutcome::Completed));
    assert_eq!(alone.ending(), Some(Ending::Failed));
}

#[test]
fn the_launch_waits_for_every_run_the_operation_started() {
    let mut driver = driver();
    driver.on_event(&snapshot());
    driver.on_event(&started("first"));
    driver.on_event(&started("second"));
    driver.on_event(&message("agent", "Done."));
    driver.on_event(&finished("first", RunOutcome::Completed));
    driver.on_event(&operation(RunOutcome::Completed));
    assert_eq!(driver.ending(), None);
    driver.on_event(&finished("second", RunOutcome::Completed));
    assert_eq!(driver.ending(), Some(Ending::Completed));
}

#[test]
fn a_refused_message_fails_the_launch_and_says_why() {
    let mut driver = driver();
    driver.on_event(&snapshot());
    let steps = driver.on_event(&Event::Reply {
        request: RequestId::new(1),
        error: Some(Rejection::new(ErrorCode::Unavailable, "No agent is ready")),
    });
    assert_eq!(
        steps,
        [Step::Tell(
            "The message was refused: No agent is ready".into()
        )]
    );
    assert_eq!(driver.ending(), Some(Ending::Failed));
}

#[test]
fn every_approval_is_refused_since_nobody_can_answer_it() {
    let mut driver = driver();
    driver.on_event(&snapshot());
    driver.on_event(&started("run"));
    let permission = Permission::new("p1", "agent", "run", "Run a shell command", "cargo test");
    let steps = driver.on_event(&Event::PermissionRequested {
        conversation: "room".into(),
        permission,
    });
    assert_eq!(
        sent(&steps),
        [&Command::Decide {
            permission: "p1".into(),
            decision: Decision::Deny
        }]
    );
    assert!(steps.contains(&Step::Tell(
        "Refused, nobody can approve it: Run a shell command".into()
    )));
    // A refused decision of its own does not fail the message.
    let steps = driver.on_event(&Event::Reply {
        request: RequestId::new(2),
        error: Some(Rejection::new(ErrorCode::Invalid, "Already settled")),
    });
    assert_eq!(
        steps,
        [Step::Tell(
            "A request to the conversation was declined: Already settled".into()
        )]
    );
    driver.on_event(&message("agent", "Done without the shell."));
    driver.on_event(&finished("run", RunOutcome::Completed));
    driver.on_event(&operation(RunOutcome::Completed));
    assert_eq!(driver.ending(), Some(Ending::Completed));
}

#[test]
fn a_signal_cuts_the_running_turns_then_ends_interrupted() {
    let mut driver = driver();
    driver.on_event(&snapshot());
    driver.on_event(&started("run"));
    let steps = driver.interrupt();
    assert_eq!(sent(&steps), [&Command::Interrupt { run: "run".into() }]);
    // The cut turn still reports its end before the launch ends.
    assert_eq!(driver.ending(), None);
    driver.on_event(&finished("run", RunOutcome::Interrupted));
    driver.on_event(&operation(RunOutcome::Interrupted));
    assert_eq!(driver.ending(), Some(Ending::Interrupted));
}

#[test]
fn a_signal_before_the_turn_starts_cuts_it_when_it_does() {
    let mut driver = driver();
    driver.on_event(&snapshot());
    assert!(driver.interrupt().is_empty());
    let steps = driver.on_event(&started("run"));
    assert_eq!(sent(&steps), [&Command::Interrupt { run: "run".into() }]);
    driver.on_event(&finished("run", RunOutcome::Interrupted));
    driver.on_event(&operation(RunOutcome::Interrupted));
    assert_eq!(driver.ending(), Some(Ending::Interrupted));
}

#[test]
fn a_signal_before_any_snapshot_never_sends_the_message() {
    let mut driver = driver();
    assert!(driver.interrupt().is_empty());
    assert_eq!(driver.ending(), Some(Ending::Interrupted));
    assert!(driver.on_event(&snapshot()).is_empty());
}

#[test]
fn notices_are_told_apart_from_the_answer() {
    let mut driver = driver();
    let steps = driver.on_event(&Event::Notice {
        conversation: "room".into(),
        text: "Tool round limit reached.".into(),
    });
    assert_eq!(steps, [Step::Tell("Tool round limit reached.".into())]);
    assert_eq!(driver.answer(), None);
}

#[test]
fn each_launch_sends_its_message_under_an_operation_of_its_own() {
    // The host drops an operation it already holds as a replay: a second
    // launch on the same conversation must not reuse the first one's.
    let operations: Vec<String> = ["first", "second"]
        .into_iter()
        .map(|launch| {
            let mut driver = Driver::new("Fix the build.".into(), launch.into());
            let steps = driver.on_event(&snapshot());
            let [Step::Send(request)] = steps.as_slice() else {
                panic!("one request expected: {steps:?}");
            };
            request.operation.as_str().to_owned()
        })
        .collect();
    assert_eq!(operations, ["first-message", "second-message"]);
}

#[test]
fn a_turn_that_ends_without_an_answer_fails_the_launch() {
    let mut driver = driver();
    driver.on_event(&snapshot());
    driver.on_event(&started("run"));
    driver.on_event(&finished("run", RunOutcome::Completed));
    driver.on_event(&operation(RunOutcome::Completed));
    assert_eq!(driver.ending(), Some(Ending::Failed));
}

#[test]
fn a_signal_once_the_turn_answered_changes_nothing() {
    let mut driver = driver();
    driver.on_event(&snapshot());
    driver.on_event(&started("run"));
    driver.on_event(&message("agent", "Done."));
    driver.on_event(&finished("run", RunOutcome::Completed));
    // The signal lands while the operation settles, after the turn ended.
    assert!(driver.interrupt().is_empty());
    driver.on_event(&operation(RunOutcome::Completed));
    assert_eq!(driver.ending(), Some(Ending::Completed));
}
