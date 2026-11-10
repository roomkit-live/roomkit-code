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
        (json!({}), "takes `questions`"),
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
    ] {
        let refused = questions(&arguments(value)).unwrap_err();
        assert!(refused.contains(expected), "{refused}");
    }
}

#[test]
fn the_tool_offers_what_the_reader_accepts() {
    let tool = definition();
    assert_eq!(tool.name, ASK_USER);
    let questions = &tool.parameters["properties"]["questions"];
    assert_eq!(questions["maxItems"], MAX_QUESTIONS);
    assert_eq!(
        questions["items"]["properties"]["options"]["maxItems"],
        MAX_OPTIONS
    );
    assert!(tool.description.contains("(Recommended)"));
    assert!(handler().is_ok());
}
