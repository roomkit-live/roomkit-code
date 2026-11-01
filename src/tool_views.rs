//! Development-tool presentation belongs to the product, not the generic TUI.

mod changes;
mod docs;
mod plans;
mod processes;
mod skills;
mod web;

use roomkit_tools_code::{CodeLanguage, CodeOutlinePage};
use roomkit_tools_fs::{GrepPage, PathKind, PathPage, ReadPage};
use roomkit_tui::{
    content,
    protocol::{ToolCall, ToolState},
    render::{Line, Renderers, Span, Tone, ToolRenderer},
};
use serde::{Deserialize, de::DeserializeOwned};

pub(crate) fn renderers(workspace: &std::path::Path) -> Renderers {
    let mut renderers = Renderers::default();
    for name in ["list", "glob", "grep", "read", "code_outline"] {
        renderers.register(
            name,
            Files {
                workspace: workspace.to_owned(),
            },
        );
    }
    for name in ["write", "edit", "diff", "patch", "shell"] {
        renderers.register(name, changes::Changes);
    }
    for name in ["web_fetch", "web_search"] {
        renderers.register(name, web::Web);
    }
    for name in [
        "process_start",
        "process_status",
        "process_read",
        "process_write",
        "process_stop",
        "output_read",
        "output_forget",
    ] {
        renderers.register(name, processes::Processes);
    }
    for name in ["activate_skill", "read_skill_reference"] {
        renderers.register(name, skills::Skills);
    }
    for name in [
        "plan_create",
        "plan_read",
        "plan_list",
        "plan_update",
        "plan_delete",
    ] {
        renderers.register(name, plans::Plans);
    }
    for name in ["docs_list", "docs_search", "docs_read"] {
        renderers.register(name, docs::Docs);
    }
    renderers
}

#[derive(Default)]
struct Files {
    workspace: std::path::PathBuf,
}

impl Files {
    fn path(&self, path: &str) -> String {
        let relative = std::path::Path::new(path)
            .strip_prefix(&self.workspace)
            .ok();
        match relative {
            Some(path) if path.as_os_str().is_empty() => ".".into(),
            Some(path) => path.display().to_string(),
            None => path.into(),
        }
    }
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

// The current engine stores the tools' JSON envelope in their result text.
// Decode only these known tool schemas, for display only. Never promote text
// into protocol data or infer success/permissions from its contents. Prefer
// authoritative structured data when a future adapter provides it.
fn output<T: DeserializeOwned>(call: &ToolCall) -> Option<T> {
    if call.state != ToolState::Succeeded {
        return None;
    }
    if let Some(data) = &call.data {
        serde_json::from_value::<Envelope<T>>(data.clone())
            .ok()
            .map(|output| output.data)
    } else {
        serde_json::from_str::<Envelope<T>>(&call.text)
            .ok()
            .map(|output| output.data)
    }
}

impl ToolRenderer for Files {
    fn has_expanded_details(&self, call: &ToolCall) -> bool {
        match call.name.as_str() {
            "list" | "glob" => output::<PathPage>(call).is_some_and(|page| page.entries.len() > 2),
            "code_outline" => {
                output::<CodeOutlinePage>(call).is_some_and(|page| page.symbols.len() > 2)
            }
            "read" => output::<ReadPage>(call).is_some_and(|page| page.lines.len() > 2),
            "grep" => output::<GrepPage>(call).is_some_and(|page| {
                page.matches.len() > 2
                    || page.matches.iter().any(|found| {
                        !found.context_before.is_empty() || !found.context_after.is_empty()
                    })
            }),
            _ => false,
        }
    }

    fn render(&self, call: &ToolCall) -> Vec<String> {
        self.render_styled(call, false)
            .into_iter()
            .map(|line| line.into_iter().map(|span| span.text).collect())
            .collect()
    }

    fn render_styled(&self, call: &ToolCall, expanded: bool) -> Vec<Line> {
        let lines = match call.name.as_str() {
            "list" | "glob" => output::<PathPage>(call).map(|mut page| {
                page.path = self.path(&page.path);
                paths(page, expanded)
            }),
            "code_outline" => output::<CodeOutlinePage>(call).map(|mut page| {
                page.path = self.path(&page.path);
                outline(page, expanded)
            }),
            "read" => output::<ReadPage>(call).map(|mut page| {
                page.path = self.path(&page.path);
                read(page, expanded)
            }),
            "grep" => output::<GrepPage>(call).map(|mut page| {
                page.path = self.path(&page.path);
                grep(page, call, expanded)
            }),
            _ => None,
        };
        lines.unwrap_or_else(|| {
            let mut lines = vec![vec![Span::new(
                format!("args: {}", call.arguments),
                Tone::Muted,
            )]];
            if !call.text.is_empty() {
                lines.extend(content::json_or_plain(&call.text));
            } else if let Some(data) = &call.data {
                lines.extend(content::json_or_plain(&data.to_string()));
            }
            lines
        })
    }
}

fn continuation(lines: &mut Vec<Line>, truncated: bool, next: Option<usize>) {
    if truncated {
        let detail = next.map_or_else(
            || "More results available".into(),
            |next| format!("More results available · next offset {next}"),
        );
        lines.push(vec![Span::new(detail, Tone::Warning)]);
    }
}

fn paths(page: PathPage, expanded: bool) -> Vec<Line> {
    let mut lines = vec![vec![
        Span::new(page.path, Tone::Code),
        Span::new(
            format!(
                " · {} entries{}",
                page.entries.len(),
                if page.truncated {
                    " · more available"
                } else {
                    ""
                }
            ),
            Tone::Muted,
        ),
    ]];
    let limit = if expanded { 96 } else { 2 };
    for entry in page.entries.iter().take(limit) {
        let (marker, suffix, tone) = match entry.kind {
            PathKind::Directory => ("▸ ", "/", Tone::Accent),
            PathKind::File => ("  ", "", Tone::Code),
            PathKind::Symlink => ("↗ ", "", Tone::Warning),
            PathKind::Other => ("· ", "", Tone::Muted),
        };
        lines.push(vec![Span::new(
            format!("{marker}{}{suffix}", entry.path),
            tone,
        )]);
    }
    if page.entries.len() > limit {
        lines.push(vec![Span::new(
            format!(
                "… {} more entries in this result",
                page.entries.len() - limit
            ),
            Tone::Muted,
        )]);
    }
    continuation(&mut lines, page.truncated, page.next_offset);
    lines
}

fn language(path: &str) -> &str {
    std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
}

fn read(page: ReadPage, expanded: bool) -> Vec<Line> {
    let range = match (page.lines.first(), page.lines.last()) {
        (Some(first), Some(last)) => format!(" · lines {}–{}", first.number, last.number),
        _ => " · empty file".into(),
    };
    let mut lines = vec![vec![
        Span::new(&page.path, Tone::Code),
        Span::new(range, Tone::Muted),
    ]];
    let limit = if expanded { 96 } else { 2 };
    let selected: Vec<_> = page.lines.iter().take(limit).collect();
    let source = selected
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let numbered = content::code(&source, language(&page.path));
    for (line, mut spans) in selected.iter().zip(numbered) {
        spans.insert(0, Span::new(format!("{:>4} │ ", line.number), Tone::Muted));
        lines.push(spans);
    }
    if page.lines.len() > limit {
        lines.push(vec![Span::new(
            format!("… {} more lines in this result", page.lines.len() - limit),
            Tone::Muted,
        )]);
    }
    continuation(&mut lines, page.truncated, page.next_offset);
    lines
}

fn grep(page: GrepPage, call: &ToolCall, expanded: bool) -> Vec<Line> {
    let pattern = call
        .arguments
        .get("pattern")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let mut lines = vec![vec![
        Span::new(format!("{pattern:?}"), Tone::Code),
        Span::new(
            format!(
                " · {} matches in {}{}",
                page.matches.len(),
                page.path,
                if page.truncated {
                    " · more available"
                } else {
                    ""
                }
            ),
            Tone::Muted,
        ),
    ]];
    let limit = if expanded { 48 } else { 2 };
    for found in page.matches.iter().take(limit) {
        let context_before = if expanded {
            found.context_before.as_slice()
        } else {
            &[]
        };
        let context_after = if expanded {
            found.context_after.as_slice()
        } else {
            &[]
        };
        for line in context_before {
            lines.push(vec![Span::new(
                format!("{}:{}  {}", found.path, line.number, line.text),
                Tone::Muted,
            )]);
        }
        let mut row = vec![Span::new(
            format!("{}:{}  ", found.path, found.line),
            Tone::Code,
        )];
        row.extend(
            content::code(&found.text, language(&found.path))
                .into_iter()
                .next()
                .unwrap_or_default(),
        );
        lines.push(row);
        for line in context_after {
            lines.push(vec![Span::new(
                format!("{}:{}  {}", found.path, line.number, line.text),
                Tone::Muted,
            )]);
        }
    }
    if page.matches.len() > limit {
        lines.push(vec![Span::new(
            format!(
                "… {} more matches in this result",
                page.matches.len() - limit
            ),
            Tone::Muted,
        )]);
    }
    if page.skipped_files > 0 {
        lines.push(vec![Span::new(
            format!("{} binary or non-UTF-8 files skipped", page.skipped_files),
            Tone::Warning,
        )]);
    }
    continuation(&mut lines, page.truncated, page.next_offset);
    lines
}

fn outline(page: CodeOutlinePage, expanded: bool) -> Vec<Line> {
    let language = match page.language {
        CodeLanguage::Rust => "rust",
        CodeLanguage::Python => "python",
    };
    let mut lines = vec![vec![
        Span::new(&page.path, Tone::Code),
        Span::new(
            format!(" · {language} · {} symbols", page.total_symbols),
            Tone::Muted,
        ),
    ]];
    if page.syntax_errors {
        lines.push(vec![Span::new(
            "Syntax errors detected; this outline may be incomplete",
            Tone::Warning,
        )]);
    }
    let limit = if expanded { 96 } else { 2 };
    for symbol in page.symbols.iter().take(limit) {
        let mut heading = vec![Span::new(
            format!("{}–{} │ ", symbol.start_line, symbol.end_line),
            Tone::Muted,
        )];
        let signature = symbol.signature.replace(['\n', '\r'], " ");
        heading.extend(content::code(&signature, language).into_iter().flatten());
        if symbol.signature_truncated {
            heading.push(Span::new(" … [header truncated]", Tone::Warning));
        }
        lines.push(heading);
    }
    if page.symbols.len() > limit {
        lines.push(vec![Span::new(
            format!(
                "… {} more symbols in this result",
                page.symbols.len() - limit
            ),
            Tone::Muted,
        )]);
    }
    if let Some(offset) = page.next_offset {
        lines.push(vec![Span::new(
            format!("More symbols available · code_outline offset={offset}"),
            Tone::Warning,
        )]);
    } else if page.truncated {
        lines.push(vec![Span::new(
            "Outline truncated; inspect the source for further definitions",
            Tone::Warning,
        )]);
    }
    lines
}

#[cfg(test)]
mod tests {
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
            json!({"path":".","matches":[{"path":"src/lib.rs","line":7,"text":"pub fn answer() -> u8 { 42 }","context_before":[{"number":6,"text":"// explanation"}]}],"truncated":false,"next_offset":null,"skipped_files":1,"skipped_entries":0,"skipped_lines":0}),
        );
        let rendered = Files::default().render_styled(&grep, true);
        assert!(
            plain(&rendered).contains("src/lib.rs:7") && plain(&rendered).contains("src/lib.rs:6")
        );
        assert!(plain(&rendered).contains("1 binary or non-UTF-8 files skipped"));
    }

    #[test]
    fn refusals_and_unrecognized_payloads_keep_their_actual_output() {
        let mut call = call("list", json!({"unexpected":"payload"}));
        assert!(plain(&Files::default().render_styled(&call, true)).contains("payload"));
        call.state = ToolState::Denied;
        call.text = "Reading is disabled".into();
        assert!(
            plain(&Files::default().render_styled(&call, false)).contains("Reading is disabled")
        );
        assert!(!plain(&Files::default().render_styled(&call, false)).contains("entries"));
        assert_eq!(call.state, ToolState::Denied);
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
}
