//! Process state, stream cursors and ephemeral archives, without raw byte arrays.
use super::{
    changes::{Changes, excerpt},
    output,
};
use roomkit_tools_process::{
    OutputArchive, OutputForgetResult, OutputPage, ProcessChunk, ProcessReadResult, ProcessState,
    ProcessStatus, ProcessStatusResult, ProcessStopResult, ProcessWriteResult,
};
use roomkit_tui::{
    protocol::ToolCall,
    render::{Line, Span, Tone, ToolRenderer},
};

pub(super) struct Processes;

impl ToolRenderer for Processes {
    fn activity(&self, call: &ToolCall) -> Option<String> {
        super::labels::activity(call)
    }

    fn render(&self, call: &ToolCall) -> Vec<String> {
        self.render_styled(call, false)
            .into_iter()
            .map(|line| line.into_iter().map(|s| s.text).collect())
            .collect()
    }

    fn render_styled(&self, call: &ToolCall, expanded: bool) -> Vec<Line> {
        let mut lines = Vec::new();
        let limit = if expanded { 96 } else { 2 };
        match call.name.as_str() {
            "process_start" => {
                if let Some(result) = output::<ProcessStatus>(call) {
                    if let Some(command) = call
                        .arguments
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                    {
                        lines.push(vec![Span::new(format!("$ {command}"), Tone::Code)]);
                    }
                    status(&mut lines, &result);
                }
            }
            "process_status" => {
                if let Some(result) = output::<ProcessStatusResult>(call) {
                    lines.push(vec![Span::new(
                        format!("{} retained processes", result.processes.len()),
                        Tone::Muted,
                    )]);
                    for result in result.processes {
                        status(&mut lines, &result);
                    }
                }
            }
            "process_read" => {
                if let Some(result) = output::<ProcessReadResult>(call) {
                    status(&mut lines, &result.status);
                    chunk(&mut lines, "stdout", &result.stdout, limit);
                    chunk(&mut lines, "stderr", &result.stderr, limit);
                }
            }
            "process_write" => {
                if let Some(result) = output::<ProcessWriteResult>(call) {
                    lines.push(vec![Span::new(
                        format!(
                            "{} · {} bytes sent · {}{}",
                            result.process_id,
                            result.bytes_written,
                            if result.complete {
                                "delivered to pipe"
                            } else {
                                "partial delivery"
                            },
                            if result.stdin_closed {
                                " · stdin closed"
                            } else {
                                ""
                            }
                        ),
                        if result.complete {
                            Tone::Success
                        } else {
                            Tone::Warning
                        },
                    )]);
                    if let Some(error) = result.error {
                        lines.push(vec![Span::new(error, Tone::Error)]);
                    }
                }
            }
            "process_stop" => {
                if let Some(result) = output::<ProcessStopResult>(call) {
                    status(&mut lines, &result.status);
                    if result.forgotten {
                        lines.push(vec![Span::new("Process record released", Tone::Muted)]);
                    }
                }
            }
            "output_read" => {
                if let Some(page) = output::<OutputPage>(call) {
                    archive(&mut lines, "output", &page.archive);
                    lines.push(vec![Span::new(
                        format!(
                            "bytes {}–{}{}",
                            page.offset,
                            page.next_offset,
                            if page.has_more {
                                " · more available"
                            } else {
                                ""
                            }
                        ),
                        Tone::Muted,
                    )]);
                    excerpt(&mut lines, &page.text, "", Tone::Text, limit);
                }
            }
            "output_forget" => {
                if let Some(result) = output::<OutputForgetResult>(call) {
                    lines.push(vec![Span::new(
                        format!(
                            "{} · {}",
                            result.archive_id,
                            if result.forgotten {
                                "archive released"
                            } else {
                                "release pending"
                            }
                        ),
                        Tone::Muted,
                    )]);
                }
            }
            _ => {}
        }
        if lines.is_empty() {
            Changes.render_styled(call, expanded)
        } else {
            lines
        }
    }
}

fn status(lines: &mut Vec<Line>, s: &ProcessStatus) {
    let state = match s.state {
        ProcessState::Running => "running",
        ProcessState::Finished => "finished",
    };
    lines.push(vec![
        Span::new(&s.process_id, Tone::Code),
        Span::new(
            format!(
                " · {state}{}{}",
                s.end.map(|end| format!(" · {end:?}")).unwrap_or_default(),
                s.exit_code
                    .map(|code| format!(" · exit {code}"))
                    .unwrap_or_default()
            ),
            if s.exit_code == Some(0) {
                Tone::Success
            } else {
                Tone::Muted
            },
        ),
    ]);
    if s.state == ProcessState::Finished && !s.output_complete {
        lines.push(vec![Span::new("Incomplete output capture", Tone::Warning)]);
    }
    if let Some(error) = &s.error {
        lines.push(vec![Span::new(error, Tone::Error)]);
    }
    if let Some(a) = &s.stdout_archive {
        archive(lines, "stdout", a);
    }
    if let Some(a) = &s.stderr_archive {
        archive(lines, "stderr", a);
    }
}

fn chunk(lines: &mut Vec<Line>, name: &str, c: &ProcessChunk, limit: usize) {
    lines.push(vec![Span::new(
        format!(
            "{name} · bytes {}–{} / {}{}",
            c.offset,
            c.next_offset,
            c.total_bytes,
            if c.has_more { " · more available" } else { "" }
        ),
        Tone::Muted,
    )]);
    excerpt(
        lines,
        &c.text,
        "",
        if name == "stderr" {
            Tone::Warning
        } else {
            Tone::Text
        },
        limit,
    );
    if c.omitted_bytes > 0 {
        lines.push(vec![Span::new(
            format!("{} bytes no longer in the process buffer", c.omitted_bytes),
            Tone::Warning,
        )]);
    }
}

pub(super) fn archive(lines: &mut Vec<Line>, name: &str, a: &OutputArchive) {
    if a.observed_bytes == 0 && a.error.is_none() {
        return;
    }
    let state = if a.complete {
        "complete"
    } else if a.finished {
        "partial"
    } else {
        "capturing"
    };
    lines.push(vec![Span::new(
        format!(
            "{name} archive {} · {} / {} bytes · {state}",
            a.archive_id.as_deref().unwrap_or("unavailable"),
            a.stored_bytes,
            a.observed_bytes
        ),
        if a.error.is_some() || (a.finished && !a.complete) {
            Tone::Warning
        } else {
            Tone::Muted
        },
    )]);
    if let Some(error) = &a.error {
        lines.push(vec![Span::new(error, Tone::Warning)]);
    }
}
