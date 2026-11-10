#![allow(clippy::unwrap_used, reason = "fixture failures must fail the test")]

use serde_json::json;

use super::*;

fn arguments(value: Value) -> Map<String, Value> {
    let Value::Object(map) = value else {
        unreachable!("an object fixture");
    };
    map
}

#[test]
fn a_call_s_questions_are_read_with_their_options() {
    let items = questions(&arguments(json!({"questions": [
        {"header": "Database choice", "question": " Which database? ", "options": [
            {"label": "SQLite (Recommended)", "description": "One file"},
            {"label": "Postgres"}
        ]},
        {"question": "Which port?", "multi_select": true}
    ]})))
    .unwrap();
    assert_eq!(
        items[0].header, "Database cho",
        "headers are cut at 12 characters"
    );
    assert_eq!(items[0].text, "Which database?");
    assert_eq!(items[0].options[0].label, "SQLite (Recommended)");
    assert_eq!(items[0].options[1].description, "");
    assert!(items[1].options.is_empty() && items[1].multi_select);
}

#[test]
fn what_cannot_be_shown_is_refused_with_what_to_fix() {
    let five: Vec<Value> = (0..5)
        .map(|n| json!({"question": format!("Q{n}?")}))
        .collect();
    for (value, expected) in [
        (json!({}), "missing field `questions`"),
        (json!({"questions": []}), "one to 4 questions"),
        (json!({"questions": five}), "5 were given"),
        (
            json!({"questions": [{"question": " "}]}),
            "Question 1 has no text",
        ),
        (
            json!({"questions": [{"question": "Q?", "options": [{"label": "A"}, {"label": "A"}]}]}),
            "distinct",
        ),
        (
            json!({"questions": [{"question": "Q?", "options": [{"label": "1"}, {"label": "2"}, {"label": "3"}, {"label": "4"}, {"label": "5"}]}]}),
            "at most 4",
        ),
        // One byte past the card's limits, which roomkit-tools must keep.
        (
            json!({"questions": [{"question": "Q?", "options": [{"label": "x".repeat(257)}]}]}),
            "exceeds 256 bytes",
        ),
        (
            json!({"questions": [{"question": "Q?", "options": [{"label": "A", "description": "x".repeat(513)}]}]}),
            "exceeds 512 bytes",
        ),
    ] {
        let refused = questions(&arguments(value)).unwrap_err();
        assert!(refused.contains(expected), "{refused}");
        assert!(!refused.starts_with("Invalid input"), "{refused}");
    }
}

#[test]
fn a_question_too_long_for_the_card_is_refused_with_what_to_fix() {
    let fits = "é".repeat(512);
    assert!(questions(&arguments(json!({"questions": [{"question": fits}]}))).is_ok());
    let long = format!("{fits}?");
    let refused = questions(&arguments(json!({"questions": [{"question": long}]}))).unwrap_err();
    assert_eq!(refused, "Question 1's text exceeds 1024 bytes.");
}

#[test]
fn the_largest_call_the_tool_accepts_fits_the_card() {
    let option = |n: usize| json!({"label": format!("{n}{}", "x".repeat(255)), "description": "x".repeat(512)});
    let question = |n: usize| {
        json!({
            "question": format!("{n}{}", "x".repeat(1023)),
            "header": "x".repeat(64),
            "options": (0..4).map(option).collect::<Vec<_>>(),
        })
    };
    let largest = json!({"questions": (0..4).map(question).collect::<Vec<_>>()});
    let items = questions(&arguments(largest)).unwrap();
    let card = roomkit_tui::protocol::Question::new("question", "assistant", "run", items);
    assert!(card.fits(), "the tool's limits are the card's");
}

#[test]
fn the_tool_offers_what_the_reader_accepts() {
    let handler = handler().unwrap();
    let [tool] = handler.tools().try_into().unwrap();
    assert_eq!(tool.name, ASK_USER);
    assert_eq!(tool.parameters, AskUserTool::definition().input_schema);
    assert!(tool.description.contains("(Recommended)"));
    assert_eq!(handler.timeout(), WAIT);
}
