#![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
use super::*;
use roomkit::model::{ChannelId, ChannelType, EventSource, RoomId, TextContent};
use serde_json::json;

fn said(channel: &str, text: &str) -> RoomEvent {
    let source = EventSource::new(ChannelId::new(channel).unwrap(), ChannelType::AI);
    RoomEvent::new(RoomId::new("room").unwrap(), source, TextContent::new(text))
}

/// `event` at `index` of the timeline.
fn at(index: u64, mut event: RoomEvent) -> RoomEvent {
    event.index = index;
    event
}

#[test]
fn a_summary_reads_each_message_on_its_line() {
    let events = [
        said("terminal", "First question."),
        said("code", "First answer."),
        said("terminal", " "),
        said("terminal", "Second question."),
    ];
    assert_eq!(
        lines((&events, &[], "code")),
        [
            "user: First question.",
            "assistant: First answer.",
            "user: Second question."
        ]
    );
    let long = [said("terminal", &"é".repeat(LINE_CHARS + 5))];
    assert!(lines((&long, &[], "code"))[0].ends_with("é […]"));
}

/// A request's lines take what the rest of it leaves, the longest
/// earlier summary counted, and half the budget at least.
#[test]
fn a_request_s_lines_take_what_the_rest_leaves_and_half_at_least() {
    let longest = format!("{}\n\n{CUT}", "p".repeat(SUMMARY_BYTES));
    let whole = ask(Some(&longest)).len() + fence("conversation", "").len();
    assert_eq!(asked(), whole);
    assert_eq!(room(asked() + 50_000), 50_000);
    assert_eq!(room(16_000), 8_000);
}

/// A turn made of calls and one answer left a summary its question and
/// that answer: what it read, changed and ran was lost with its rows.
#[test]
fn a_summary_reads_each_call_at_its_place_among_the_messages() {
    let events = [
        at(1, said("terminal", "Fix the test.")),
        at(6, said("code", "Fixed.")),
        at(7, said("terminal", "Thanks.")),
    ];
    let calls = [
        (5, "tool: Run cargo test → exit_code=0".to_owned()),
        (3, "tool: Edit src/lib.rs → ok".to_owned()),
    ];
    assert_eq!(
        lines((&events, &calls, "code")).join("\n"),
        "user: Fix the test.\ntool: Edit src/lib.rs → ok\ntool: Run cargo test → exit_code=0\nassistant: Fixed.\nuser: Thanks."
    );
}

fn call(name: &str, arguments: Value) -> ToolCallContent {
    let mut call = ToolCallContent::new(name, "call-1");
    call.arguments = arguments.as_object().cloned().unwrap();
    call.outcome = Some(ToolCallOutcome::Served);
    call
}

#[test]
fn a_call_s_line_says_what_it_was_and_how_it_ended_never_its_result() {
    let mut read = call("read", json!({"path": "src/lib.rs"}));
    read.result = json!("1: SECRET_FILE_TEXT\n2: more");
    assert_eq!(call_line(&read), "tool: Read src/lib.rs → ok");

    // A command says what it is for, and how it exited.
    let mut shell = call("shell", json!({"command": "cargo test\n  --locked"}));
    shell.intent = Some("Run the tests".into());
    shell.result = json!("exit_code=101\nstdout:\nTEST_OUTPUT");
    assert_eq!(
        call_line(&shell),
        "tool: Run cargo test --locked (Run the tests) → exit_code=101"
    );
    // Its result may come beside the nested rules of what it read.
    shell.result = json!({"project_instructions": "RULES", "text": "exit_code=0\nstdout:"});
    assert!(call_line(&shell).ends_with("→ exit_code=0"));

    let mut missing = call("read", json!({"path": "absent.rs"}));
    missing.outcome = Some(ToolCallOutcome::Failed);
    missing.error = Some("Tool 'read' failed (ToolIOError)\nmore".into());
    assert_eq!(
        call_line(&missing),
        "tool: Read absent.rs → failed: Tool 'read' failed (ToolIOError)"
    );
    let mut refused = call("write", json!({"path": "a.rs", "content": "FILE_TEXT"}));
    refused.outcome = Some(ToolCallOutcome::Refused);
    refused.result = json!("The action was not approved.");
    assert_eq!(
        call_line(&refused),
        "tool: Write a.rs → refused: The action was not approved."
    );
    // A row from before the kit recorded outcomes: its error tells.
    let mut old = call("grep", json!({"pattern": "x"}));
    old.outcome = None;
    old.error = Some("Invalid input: invalid regex".into());
    assert!(call_line(&old).ends_with("→ failed: Invalid input: invalid regex"));
}

#[test]
fn a_long_command_and_a_long_error_are_cut() {
    let mut shell = call("shell", json!({"command": "x".repeat(500)}));
    shell.intent = Some("i".repeat(200));
    shell.outcome = Some(ToolCallOutcome::Failed);
    shell.error = Some("e".repeat(500));
    let line = call_line(&shell);
    let most = "tool: Run ".len() + CALL_CHARS + INTENT_CHARS + CALL_CHARS + 40;
    assert!(line.chars().count() < most, "{}", line.chars().count());
    assert!(line.contains(&format!("{}…", "x".repeat(CALL_CHARS - "Run ".len()))));
}

/// A summary cut at its output budget ended mid-sentence, and nothing
/// said so to the agent that read it, nor to the person.
#[test]
fn a_summary_cut_short_is_kept_and_says_so() {
    assert!(written("  \n", Some(FinishReason::Stop)).is_none());
    let whole = written(" The user asked for tests. ", Some(FinishReason::Stop)).unwrap();
    assert_eq!(
        (whole.text.as_str(), whole.cut),
        ("The user asked for tests.", false)
    );
    // An output that ran out of room, in the contract's word: each
    // provider maps its own onto it.
    let cut = written("The user asked for", Some(FinishReason::Length)).unwrap();
    assert!(cut.cut);
    assert_eq!(cut.text, format!("The user asked for\n\n{CUT}"));
    // A summary longer than every later request should read.
    let long = written(&"é".repeat(SUMMARY_BYTES), None).unwrap();
    assert!(long.cut && long.text.ends_with(CUT));
    assert!(long.text.len() <= SUMMARY_BYTES + CUT.len() + 2);
}

/// A part's summary cut short is carried over without its end: the
/// summary that stands for them all says so, whichever part was cut.
#[tokio::test]
async fn a_summary_in_parts_says_so_when_any_part_was_cut() {
    use roomkit_ai::testing::{ScriptedProvider, ScriptedRound};
    let provider = ScriptedProvider::new([
        ScriptedRound::answering("The user asked for").with_finish_reason(FinishReason::Length),
        ScriptedRound::answering("They fixed the test."),
    ]);
    let events: Vec<RoomEvent> = (0..20)
        .map(|_| said("terminal", &"a".repeat(1_500)))
        .collect();
    // Room for 13 of the 20 lines per request.
    let chars = asked() + 20_000;
    let written = summarize(&provider, None, (&events, &[], "code"), chars)
        .await
        .unwrap();
    assert_eq!(provider.requests().len(), 2);
    assert_eq!(written.text, "They fixed the test.");
    assert!(written.cut, "the first part was cut");
}

/// Review follow-up: nothing bounded the requests a summary was written
/// in. A history far past the budget, after `/model` moved it to a small
/// window, took one request per budget's worth of lines; under a ceiling
/// its earlier summary nearly filled, one per line. A summary now takes
/// a few requests at most, every line cut shorter to fit them.
#[tokio::test]
async fn a_summary_takes_a_few_requests_at_most_its_lines_cut_shorter() {
    use super::super::parts::MOST;
    use roomkit_ai::testing::{ScriptedProvider, ScriptedRound};
    let events: Vec<RoomEvent> = (0..200)
        .map(|at| said("terminal", &format!("Message {at}: {}", "x".repeat(1_000))))
        .collect();
    // Under a ceiling the earlier summary nearly fills, fewer lines: cut
    // to their shortest, 200 would still take more requests.
    let cases = [
        (None, 40_000, 200),
        (Some("p".repeat(SUMMARY_BYTES)), 16_000, 40),
    ];
    for (prior, chars, count) in cases {
        let events = &events[..count];
        let answers = (1..=50).map(|n| ScriptedRound::answering(format!("Summary {n}.")));
        let provider = ScriptedProvider::new(answers);
        let read = (events, &[][..], "code");
        let written = summarize(&provider, prior.as_deref(), read, chars)
            .await
            .unwrap();
        let requests: Vec<String> = provider
            .requests()
            .iter()
            .map(|request| match &request.messages[0].content {
                roomkit_ai::AiMessageContent::Text(text) => text.clone(),
                _ => String::new(),
            })
            .collect();
        assert!(requests.len() <= MOST, "{chars}: {}", requests.len());
        assert_eq!(written.text, format!("Summary {}.", requests.len()));
        for at in 0..count {
            let message = format!("user: Message {at}: x");
            assert!(
                requests.iter().any(|asked| asked.contains(&message)),
                "{at}"
            );
        }
        assert!(requests.iter().any(|asked| asked.contains("x […]")));
    }
}

#[test]
fn the_summary_s_message_carries_what_the_application_recorded() {
    let mut compacted = Compacted {
        from: 3,
        summary: "The user asked for tests.".into(),
        snapshot: None,
        summarized: 2,
        at: 9,
        recorded: None,
        estimate: None,
        fixed: None,
    };
    // The message's text; none for one made of parts, which it is not.
    let text = |compacted: &Compacted| match message(compacted).content {
        roomkit_ai::AiMessageContent::Text(text) => text,
        _ => String::new(),
    };
    let plain = text(&compacted);
    assert!(plain.ends_with("The user asked for tests.\n</conversation_summary>"));
    compacted.recorded = Some("Recorded by the application: a plan.".into());
    let carried = text(&compacted);
    assert!(
        carried.ends_with(
            "The user asked for tests.\n\nRecorded by the application: a plan.\n</conversation_summary>"
        ),
        "{carried}"
    );
}
