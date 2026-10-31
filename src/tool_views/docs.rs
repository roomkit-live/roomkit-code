//! The application's guide is shown as cited documentation, not as project files.
use super::*;
use roomkit_tools_docs::{DocsListResult, DocsReadResult, DocsSearchResult};

pub(super) struct Docs;

impl ToolRenderer for Docs {
    fn render(&self, call: &ToolCall) -> Vec<String> {
        self.render_styled(call, false)
            .into_iter()
            .map(|line| line.into_iter().map(|s| s.text).collect())
            .collect()
    }

    fn has_expanded_details(&self, call: &ToolCall) -> bool {
        match call.name.as_str() {
            "docs_list" => output::<DocsListResult>(call).is_some_and(|p| p.documents.len() > 2),
            "docs_search" => {
                output::<DocsSearchResult>(call).is_some_and(|p| !p.matches.is_empty())
            }
            "docs_read" => output::<DocsReadResult>(call).is_some_and(|p| p.lines.len() > 2),
            _ => false,
        }
    }

    fn render_styled(&self, call: &ToolCall, expanded: bool) -> Vec<Line> {
        let limit = if expanded { 96 } else { 2 };
        let lines = match call.name.as_str() {
            "docs_list" => output::<DocsListResult>(call).map(|page| list(page, limit)),
            "docs_search" => output::<DocsSearchResult>(call).map(|page| search(page, expanded)),
            "docs_read" => output::<DocsReadResult>(call).map(|page| read(page, limit)),
            _ => None,
        };
        lines.unwrap_or_else(|| content::json_or_plain(&call.text))
    }
}

fn list(page: DocsListResult, limit: usize) -> Vec<Line> {
    let mut lines = vec![vec![Span::new(
        format!("{} guide documents", page.total_documents),
        Tone::Muted,
    )]];
    for document in page.documents.iter().take(limit) {
        lines.push(vec![
            Span::new(&document.title, Tone::Text),
            Span::new(format!(" · {}", document.document_id), Tone::Muted),
        ]);
    }
    continuation(&mut lines, page.truncated, page.next_offset);
    lines
}

fn search(page: DocsSearchResult, expanded: bool) -> Vec<Line> {
    let mut lines = vec![vec![Span::new(
        format!(
            "{} of {} guide documents match",
            page.matched_documents, page.total_documents
        ),
        Tone::Muted,
    )]];
    for found in page.matches.iter().take(if expanded { 50 } else { 2 }) {
        let place = found
            .line
            .map_or_else(String::new, |line| format!(":{line}"));
        lines.push(vec![
            Span::new(&found.document.title, Tone::Text),
            Span::new(
                format!(" · {}{place}", found.document.document_id),
                Tone::Muted,
            ),
        ]);
        if expanded {
            lines.push(vec![Span::new(format!("  {}", found.text), Tone::Muted)]);
        }
    }
    continuation(&mut lines, page.truncated, page.next_offset);
    lines
}

fn read(page: DocsReadResult, limit: usize) -> Vec<Line> {
    let range = match (page.lines.first(), page.lines.last()) {
        (Some(first), Some(last)) => format!(" · lines {}–{}", first.number, last.number),
        _ => " · empty document".into(),
    };
    let mut lines = vec![vec![
        Span::new(&page.document.title, Tone::Heading),
        Span::new(
            format!(
                " · {}{range} of {}",
                page.document.document_id, page.document.line_count
            ),
            Tone::Muted,
        ),
    ]];
    for line in page.lines.iter().take(limit) {
        lines.push(vec![Span::new(&line.text, Tone::Text)]);
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
