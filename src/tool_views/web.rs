//! Public sources are presented as sources, never as trusted instructions.
use super::{
    changes::{Changes, excerpt},
    output,
};
use roomkit_tools_search::{SearchHit, SearchPayload, SearchResult};
use roomkit_tools_web::WebFetchResult;
use roomkit_tui::{
    protocol::ToolCall,
    render::{Line, Span, Tone, ToolRenderer},
};

pub(super) struct Web;

impl ToolRenderer for Web {
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
        if call.name == "web_fetch"
            && let Some(page) = output::<WebFetchResult>(call)
        {
            lines.push(vec![
                Span::new(&page.final_url, Tone::Code),
                Span::new(
                    format!(
                        " · HTTP {} · {} / {} bytes{}",
                        page.status,
                        page.content.len(),
                        page.content_bytes,
                        if page.truncated { " · truncated" } else { "" }
                    ),
                    if page.http_ok && !page.truncated {
                        Tone::Success
                    } else {
                        Tone::Warning
                    },
                ),
            ]);
            if page.url != page.final_url {
                lines.push(vec![Span::new(
                    format!("Redirected from {}", page.url),
                    Tone::Muted,
                )]);
            }
            excerpt(&mut lines, &page.content, "", Tone::Text, limit);
            if page.truncated {
                lines.push(vec![Span::new(
                    format!("Response truncated · {} bytes omitted", page.omitted_bytes),
                    Tone::Warning,
                )]);
            }
        } else if call.name == "web_search"
            && let Some(result) = output::<SearchResult>(call)
        {
            lines.push(vec![Span::new(
                format!("{} · {}", result.provider, result.query),
                Tone::Code,
            )]);
            match result.payload {
                SearchPayload::Results { results } => {
                    lines.push(vec![Span::new(
                        format!("{} sources", results.len()),
                        Tone::Muted,
                    )]);
                    sources(&mut lines, &results, expanded);
                }
                SearchPayload::Answer {
                    answer,
                    sources: hits,
                    ..
                } => {
                    excerpt(&mut lines, &answer, "", Tone::Text, limit);
                    sources(&mut lines, &hits, expanded);
                }
                SearchPayload::NotPerformed { reason } => lines.push(vec![Span::new(
                    format!("Search not performed: {reason}"),
                    Tone::Warning,
                )]),
            }
            if result.partial || result.omitted_results > 0 {
                lines.push(vec![Span::new(
                    format!(
                        "Partial results · {} sources omitted",
                        result.omitted_results
                    ),
                    Tone::Warning,
                )]);
            }
            for notice in result.notices {
                lines.push(vec![Span::new(notice, Tone::Warning)]);
            }
        } else {
            return Changes.render_styled(call, expanded);
        }
        lines
    }
}

fn sources(lines: &mut Vec<Line>, hits: &[SearchHit], expanded: bool) {
    let limit = if expanded { 100 } else { 2 };
    for hit in hits.iter().take(limit) {
        if let Some(title) = &hit.title {
            lines.push(vec![Span::new(title, Tone::Accent)]);
        }
        lines.push(vec![Span::new(&hit.url, Tone::Code)]);
        if expanded && let Some(text) = &hit.content {
            excerpt(lines, text, "", Tone::Text, 16);
        }
        if hit.omitted_bytes > 0 {
            lines.push(vec![Span::new(
                format!("Excerpt truncated · {} bytes omitted", hit.omitted_bytes),
                Tone::Warning,
            )]);
        }
    }
    if hits.len() > limit {
        lines.push(vec![Span::new(
            format!("… {} more sources", hits.len() - limit),
            Tone::Muted,
        )]);
    }
}
