//! Concrete outcomes stay distinct from tool-call completion.

use roomkit_tools_fs::{DiffLineKind, DiffResult, EditResult, PatchResult};
use roomkit_tools_process::ShellResult;
use roomkit_tui::{
    content,
    protocol::ToolCall,
    render::{Line, Span, Tone, ToolRenderer},
};

use super::output;

// Display projection accepts historical writes predating directory reporting.
#[derive(serde::Deserialize)]
struct WriteResult {
    path: String,
    created: bool,
    changed: bool,
    bytes_written: usize,
    #[serde(default)]
    created_directories: Vec<String>,
}

pub(super) struct Changes;

impl ToolRenderer for Changes {
    fn activity(&self, call: &ToolCall) -> Option<String> {
        super::labels::activity(call)
    }

    fn render(&self, call: &ToolCall) -> Vec<String> {
        self.render_styled(call, false)
            .into_iter()
            .map(|line| line.into_iter().map(|span| span.text).collect())
            .collect()
    }

    fn render_styled(&self, call: &ToolCall, expanded: bool) -> Vec<Line> {
        let mut lines = Vec::new();
        let limit = if expanded { 96 } else { 2 };
        if call.name == "write"
            && let Some(result) = output::<WriteResult>(call)
        {
            lines.push(vec![
                Span::new(&result.path, Tone::Code),
                Span::new(
                    format!(
                        " · {} · {} bytes",
                        if result.created {
                            "created"
                        } else if result.changed {
                            "updated"
                        } else {
                            "unchanged"
                        },
                        result.bytes_written
                    ),
                    Tone::Success,
                ),
            ]);
            if !result.created_directories.is_empty() {
                lines.push(vec![Span::new(
                    format!(
                        "Created directories: {}",
                        result.created_directories.join(", ")
                    ),
                    Tone::Muted,
                )]);
            }
            excerpt(
                &mut lines,
                argument(call, "content"),
                "+ ",
                Tone::Success,
                limit,
            );
        } else if call.name == "edit"
            && let Some(result) = output::<EditResult>(call)
        {
            lines.push(vec![
                Span::new(&result.path, Tone::Code),
                Span::new(
                    format!(
                        " · {} replacement(s) · {}",
                        result.replacements,
                        if result.changed {
                            "updated"
                        } else {
                            "unchanged"
                        }
                    ),
                    Tone::Success,
                ),
            ]);
            excerpt(
                &mut lines,
                argument(call, "old_text"),
                "− ",
                Tone::Error,
                limit,
            );
            excerpt(
                &mut lines,
                argument(call, "new_text"),
                "+ ",
                Tone::Success,
                limit,
            );
        } else if call.name == "diff"
            && let Some(result) = output::<DiffResult>(call)
        {
            lines.push(vec![Span::new(
                format!(
                    "{} → {}",
                    argument(call, "old_path"),
                    argument(call, "new_path")
                ),
                Tone::Code,
            )]);
            diff(&mut lines, &result, limit);
        } else if call.name == "patch"
            && let Some(result) = output::<PatchResult>(call)
        {
            let label = if result.dry_run {
                "Preview only · no files changed"
            } else if result.completed {
                "Patch applied"
            } else {
                "Patch partially applied · inspect committed effects"
            };
            lines.push(vec![Span::new(
                format!("{label} · {} effects", result.applied.len()),
                if result.completed {
                    Tone::Success
                } else {
                    Tone::Warning
                },
            )]);
            for change in &result.previews {
                lines.push(vec![Span::new(
                    format!(
                        "{:?} {}{}",
                        change.operation,
                        change.path,
                        change
                            .to
                            .as_ref()
                            .map(|to| format!(" → {to}"))
                            .unwrap_or_default()
                    ),
                    Tone::Code,
                )]);
                diff(&mut lines, &change.diff, limit);
            }
            if let Some(failure) = &result.failure {
                lines.push(vec![Span::new(
                    format!("Failed operation {}: {}", failure.index, failure.message),
                    Tone::Error,
                )]);
                for effect in &result.applied {
                    lines.push(vec![Span::new(
                        format!("Committed: {:?} {}", effect.operation, effect.path),
                        Tone::Warning,
                    )]);
                }
            }
        } else if call.name == "shell"
            && let Some(result) = output::<ShellResult>(call)
        {
            lines.push(vec![Span::new(
                format!("$ {}", argument(call, "command")),
                Tone::Code,
            )]);
            let success =
                result.exit_code == Some(0) && !result.timed_out && result.output_complete;
            lines.push(vec![Span::new(
                format!(
                    "exit {}{}{}{}",
                    result
                        .exit_code
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "unknown".into()),
                    result
                        .signal
                        .map(|signal| format!(" · signal {signal}"))
                        .unwrap_or_default(),
                    if result.timed_out {
                        " · timed out"
                    } else {
                        ""
                    },
                    if result.output_complete {
                        ""
                    } else {
                        " · incomplete output"
                    }
                ),
                if success {
                    Tone::Success
                } else {
                    Tone::Warning
                },
            )]);
            // Keep recovery handles in the preview before potentially long output.
            for (name, captured) in [("stdout", &result.stdout), ("stderr", &result.stderr)] {
                if let Some(archive) = &captured.archive {
                    super::processes::archive(&mut lines, name, archive);
                }
            }
            for (name, captured, tone) in [
                ("stdout", &result.stdout, Tone::Text),
                ("stderr", &result.stderr, Tone::Warning),
            ] {
                if !captured.text.is_empty() {
                    lines.push(vec![Span::new(name, Tone::Muted)]);
                    excerpt(&mut lines, &captured.text, "", tone, limit);
                }
                if captured.truncated {
                    lines.push(vec![Span::new(
                        format!("{name}: {} bytes omitted", captured.omitted_bytes),
                        Tone::Warning,
                    )]);
                }
            }
        } else {
            lines.push(vec![Span::new(
                format!("args: {}", call.arguments),
                Tone::Muted,
            )]);
            if !call.text.is_empty() {
                lines.extend(content::json_or_plain(&call.text));
            } else if let Some(data) = &call.data {
                lines.extend(content::json_or_plain(&data.to_string()));
            }
        }
        lines
    }
}

fn argument<'a>(call: &'a ToolCall, name: &str) -> &'a str {
    call.arguments
        .get(name)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

pub(super) fn excerpt(lines: &mut Vec<Line>, text: &str, prefix: &str, tone: Tone, limit: usize) {
    for line in text.lines().take(limit) {
        lines.push(vec![Span::new(format!("{prefix}{line}"), tone)]);
    }
    let omitted = text.lines().count().saturating_sub(limit);
    if omitted > 0 {
        lines.push(vec![Span::new(
            format!("… {omitted} more lines"),
            Tone::Muted,
        )]);
    }
}

fn diff(lines: &mut Vec<Line>, diff: &DiffResult, limit: usize) {
    lines.push(vec![
        Span::new(format!("+{}", diff.additions), Tone::Success),
        Span::new(format!(" −{}", diff.deletions), Tone::Error),
    ]);
    for line in diff.hunks.iter().flat_map(|hunk| &hunk.lines).take(limit) {
        let (prefix, tone) = match line.kind {
            DiffLineKind::Added => ("+", Tone::Success),
            DiffLineKind::Removed => ("−", Tone::Error),
            DiffLineKind::Context => (" ", Tone::Muted),
        };
        lines.push(vec![Span::new(
            format!("{prefix} {}", line.text.trim_end_matches(['\r', '\n'])),
            tone,
        )]);
    }
    let omitted = diff
        .hunks
        .iter()
        .map(|hunk| hunk.lines.len())
        .sum::<usize>()
        .saturating_sub(limit);
    if omitted > 0 {
        lines.push(vec![Span::new(
            format!("… {omitted} more diff lines"),
            Tone::Muted,
        )]);
    }
}
