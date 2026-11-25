//! Development-tool presentation belongs to the product, not the generic TUI.

mod changes;
mod docs;
mod labels;
mod plans;
mod processes;
mod skills;
mod web;

use roomkit_tools_code::{CodeLanguage, CodeOutlinePage};
use roomkit_tools_fs::{GrepMatch, GrepPage, PathKind, PathPage, ReadLine, ReadPage};
use roomkit_tui::{
    content,
    protocol::{ToolCall, ToolState},
    render::{Line, Renderers, Span, Tone, ToolRenderer},
};
use serde::{Deserialize, de::DeserializeOwned};

pub(crate) use labels::plain_label;

pub(crate) fn renderers(workspace: &std::path::Path) -> Renderers {
    let mut renderers = Renderers::default();
    renderers.set_message_emphasis(crate::messages::emphasis);
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

// A tool's data reaches the views as its call's structured copy; a session
// stored before that holds the tools' JSON envelope in its result text.
// Decode only these known tool schemas, for display only. Never promote text
// into protocol data or infer success/permissions from its contents.
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
    fn activity(&self, call: &ToolCall) -> Option<String> {
        labels::activity(call)
    }

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
                " · {} entries{}{}",
                page.entries.len(),
                skipped_total(&[(page.skipped_entries, NON_UTF8_NAMES)]),
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
    skipped(&mut lines, &[(page.skipped_entries, NON_UTF8_NAMES)]);
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
    let left_out = [
        (page.skipped_files, BINARY_FILES),
        (page.skipped_entries, NON_UTF8_NAMES),
        (page.skipped_lines, LONG_LINES),
    ];
    let mut lines = vec![vec![
        Span::new(format!("{pattern:?}"), Tone::Code),
        Span::new(
            format!(
                " · {} matches in {}{}{}",
                page.matches.len(),
                page.path,
                skipped_total(&left_out),
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
        lines.extend(match_lines(found, expanded));
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
    skipped(&mut lines, &left_out);
    continuation(&mut lines, page.truncated, page.next_offset);
    lines
}

/// One match: its row, with its context when expanded.
fn match_lines(found: &GrepMatch, expanded: bool) -> Vec<Line> {
    let (before, after): (&[ReadLine], &[ReadLine]) = if expanded {
        (&found.context_before, &found.context_after)
    } else {
        (&[], &[])
    };
    let context = |line: &ReadLine| {
        vec![Span::new(
            format!("{}:{}  {}", found.path, line.number, line.text),
            Tone::Muted,
        )]
    };
    let mut lines: Vec<Line> = before.iter().map(context).collect();
    let mut row = vec![Span::new(
        format!("{}:{}  ", found.path, found.line),
        Tone::Code,
    )];
    // The mark leads: a clipped match is long, and the card cuts its end.
    if found.clipped {
        row.push(Span::new("clipped to fit the page · ", Tone::Warning));
    }
    row.extend(
        content::code(&found.text, language(&found.path))
            .into_iter()
            .next()
            .unwrap_or_default(),
    );
    lines.push(row);
    lines.extend(after.iter().map(context));
    lines
}

/// What a scan can leave out, in the singular and the plural.
type LeftOut = (&'static str, &'static str);
const BINARY_FILES: LeftOut = ("binary or non-UTF-8 file", "binary or non-UTF-8 files");
const NON_UTF8_NAMES: LeftOut = ("non-UTF-8 name", "non-UTF-8 names");
const LONG_LINES: LeftOut = ("line too long to search", "lines too long to search");

/// What a scan left out, so a card never reads as complete when it is not.
fn skipped(lines: &mut Vec<Line>, counts: &[(usize, LeftOut)]) {
    for &(count, (one, many)) in counts {
        if count > 0 {
            let what = if count == 1 { one } else { many };
            lines.push(vec![Span::new(
                format!("{count} {what} skipped"),
                Tone::Warning,
            )]);
        }
    }
}

/// The header's note of what was left out, so a collapsed card shows it.
fn skipped_total(counts: &[(usize, LeftOut)]) -> String {
    match counts.iter().map(|&(count, _)| count).sum::<usize>() {
        0 => String::new(),
        total => format!(" · {total} skipped"),
    }
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
mod tests;
