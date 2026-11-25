use super::*;
use serde_json::json;

fn call(name: &str, data: serde_json::Value) -> ToolCall {
    ToolCall::new(
        "call",
        name,
        json!({"pattern":"answer"}),
        ToolState::Succeeded,
    )
    .with_text(json!({"data":data,"text":"model rendering"}).to_string())
    .with_duration_ms(1)
}

fn plain(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn empty_or_fully_visible_results_have_no_additional_details() {
    let files = Files::default();
    for (name, data) in [
        (
            "glob",
            json!({"path":".","entries":[],"truncated":false,"next_offset":null,"skipped_entries":0}),
        ),
        (
            "list",
            json!({"path":".","entries":[{"name":"a","path":"a","kind":"file"}],"truncated":true,"next_offset":1,"skipped_entries":0}),
        ),
        (
            "read",
            json!({"path":"empty.txt","lines":[],"truncated":false,"next_offset":null,"revision":"fixture"}),
        ),
        (
            "read",
            json!({"path":"short.txt","lines":[{"number":1,"text":"small"}],"truncated":false,"next_offset":null,"revision":"fixture"}),
        ),
        (
            "grep",
            json!({"path":".","matches":[],"truncated":false,"next_offset":null,"skipped_files":0,"skipped_entries":0,"skipped_lines":0}),
        ),
    ] {
        let call = call(name, data);
        assert!(
            !files.has_expanded_details(&call),
            "{name} has nothing more to show"
        );
        assert_eq!(
            files.render_styled(&call, false),
            files.render_styled(&call, true)
        );
    }
    let found = call(
        "grep",
        json!({"path":".","matches":[{"path":"file","line":2,"text":"match","context_before":[{"number":1,"text":"context"}]}],"truncated":false,"next_offset":null,"skipped_files":0,"skipped_entries":0,"skipped_lines":0}),
    );
    assert!(files.has_expanded_details(&found));
}

#[test]
fn directory_presentations_are_compact_and_expansion_shows_the_full_page() {
    let call = call(
        "list",
        json!({"path":".","entries":[
        {"name":"src","path":"src","kind":"directory"},
        {"name":"Cargo.toml","path":"Cargo.toml","kind":"file"},
        {"name":"README.md","path":"README.md","kind":"file"}
    ],"truncated":true,"next_offset":4,"skipped_entries":0}),
    );
    let source = call.text.clone();
    assert!(Files::default().has_expanded_details(&call));
    let short = Files::default().render_styled(&call, false);
    assert!(plain(&short).contains("3 entries") && plain(&short).contains("▸ src/"));
    assert!(!plain(&short).contains("README.md"));
    assert!(plain(&Files::default().render_styled(&call, true)).contains("README.md"));
    assert!(plain(&short).contains("next offset 4"));
    assert_eq!(call.text, source);
    assert!(
        call.data.is_none(),
        "Display parsing must not promote text into protocol data"
    );
}

#[test]
fn source_and_search_views_show_paths_line_numbers_and_semantic_colors() {
    let read = call(
        "read",
        json!({"path":"src/lib.rs","lines":[{"number":7,"text":"pub fn answer() -> u8 { 42 }"}],"truncated":false,"next_offset":null,"revision":"fixture"}),
    );
    let rendered = Files::default().render_styled(&read, true);
    assert!(plain(&rendered).contains("src/lib.rs · lines 7–7"));
    assert!(plain(&rendered).contains("   7 │ pub fn answer()"));
    assert!(
        rendered
            .iter()
            .flatten()
            .any(|span| span.tone == Tone::Keyword)
    );
    assert!(
        rendered
            .iter()
            .flatten()
            .any(|span| span.tone == Tone::Number)
    );
    let grep = call(
        "grep",
        json!({"path":".","matches":[{"path":"src/lib.rs","line":7,"text":"pub fn answer() -> u8 { 42 }","context_before":[{"number":6,"text":"// explanation"}]},{"path":"src/big.rs","line":1,"text":format!("const DATA: &str = \"{}", "x".repeat(50_000)),"clipped":true}],"truncated":false,"next_offset":null,"skipped_files":1,"skipped_entries":2,"skipped_lines":3}),
    );
    let rendered = Files::default().render_styled(&grep, true);
    assert!(plain(&rendered).contains("src/lib.rs:7") && plain(&rendered).contains("src/lib.rs:6"));
    assert!(plain(&rendered).contains("1 binary or non-UTF-8 file skipped"));
    // A scan that left names or long lines out says so, as for files.
    assert!(plain(&rendered).contains("2 non-UTF-8 names skipped"));
    assert!(plain(&rendered).contains("3 lines too long to search skipped"));
    // The clipped mark leads its row: the card cuts a 50 KB match's end.
    let clipped = rendered.iter().any(|line| {
        line.first()
            .is_some_and(|span| span.text.starts_with("src/big.rs:1"))
            && line
                .get(1)
                .is_some_and(|span| span.text.contains("clipped"))
    });
    assert!(clipped, "{}", plain(&rendered));
    // A collapsed card, which shows two matches, still says what was left out.
    assert!(plain(&Files::default().render_styled(&grep, false)).contains("· 6 skipped"));
    let listed = call(
        "list",
        json!({"path":".","entries":[{"name":"a","path":"a","kind":"file"}],"truncated":false,"next_offset":null,"skipped_entries":4}),
    );
    let listed = plain(&Files::default().render_styled(&listed, false));
    assert!(listed.contains("4 non-UTF-8 names skipped") && listed.contains("· 4 skipped"));
}

#[test]
fn refusals_and_unrecognized_payloads_keep_their_actual_output() {
    let mut call = call("list", json!({"unexpected":"payload"}));
    assert!(plain(&Files::default().render_styled(&call, true)).contains("payload"));
    call.state = ToolState::Denied;
    call.text = "Reading is disabled".into();
    assert!(plain(&Files::default().render_styled(&call, false)).contains("Reading is disabled"));
    assert!(!plain(&Files::default().render_styled(&call, false)).contains("entries"));
    assert_eq!(call.state, ToolState::Denied);
}

#[test]
fn process_status_shows_each_listed_process() {
    let status = call(
        "process_status",
        json!({"processes":[{
            "process_id":"p1","cwd":".","state":"running","end":null,
            "exit_code":null,"signal":null,"timeout_ms":300000,
            "output_complete":false,"stdin_closed":false,
            "stdout_bytes":0,"stderr_bytes":0,
            "stdout_retained_from":0,"stderr_retained_from":0,"error":null
        }]}),
    );
    let shown = plain(&processes::Processes.render_styled(&status, false));
    assert!(
        shown.contains("1 retained processes") && shown.contains("p1 · running"),
        "{shown}"
    );
}

#[test]
fn shell_failures_and_partial_patches_remain_explicit_after_tool_completion() {
    let shell = call(
        "shell",
        json!({
            "cwd":".","exit_code":2,"signal":null,"timed_out":false,"timeout_ms":10000,
            "output_complete":true,
            "stdout":{"text":"","truncated":false,"omitted_bytes":0,"total_bytes":0},
            "stderr":{"text":"test failed","truncated":false,"omitted_bytes":0,"total_bytes":11}
        }),
    );
    let lines = changes::Changes.render_styled(&shell, false);
    assert!(plain(&lines).contains("exit 2") && plain(&lines).contains("test failed"));
    assert!(
        lines
            .iter()
            .flatten()
            .any(|span| span.text == "exit 2" && span.tone == Tone::Warning)
    );
    let preview = call(
        "patch",
        json!({"dry_run":true,"completed":true,"previews":[],"applied":[],"failure":null}),
    );
    assert!(
        plain(&changes::Changes.render_styled(&preview, false))
            .contains("Preview only · no files changed")
    );
    let partial = call(
        "patch",
        json!({"dry_run":false,"completed":false,"previews":[],"applied":[{"index":1,"operation":"add","path":"created.txt"}],"failure":{"index":2,"cancelled":false,"message":"Source changed"}}),
    );
    let lines = changes::Changes.render_styled(&partial, true);
    assert!(plain(&lines).contains("partially applied"));
    assert!(plain(&lines).contains("Committed: Add created.txt"));
    assert!(plain(&lines).contains("Source changed"));
}
