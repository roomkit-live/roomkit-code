#![expect(clippy::unwrap_used, reason = "test assertions")]

use super::*;
use roomkit_tools_docs::{
    DocsListTool, DocsReadInput, DocsReadTool, DocsSearchInput, DocsSearchTool,
};
use tokio_util::sync::CancellationToken;

fn names(topics: &[Topic]) -> Vec<&str> {
    topics.iter().map(|topic| topic.file.as_str()).collect()
}

#[test]
fn sections_become_titled_topics_and_fenced_headings_stay_in_their_section() {
    let guide = "# Product\n\nIntro.\n\n## Run it\n\n```sh\n## not a heading\n```\n\n### Detail\nText.\n\n## Run it\nAgain.\n\n## ???\n";
    let topics = topics(guide);
    assert_eq!(
        names(&topics),
        ["overview.md", "run-it.md", "run-it-2.md", "section.md"]
    );
    assert_eq!(topics[0].text, "# Product\n\nIntro.\n");
    assert_eq!(
        topics[1].text,
        "# Run it\n\n```sh\n## not a heading\n```\n\n### Detail\nText.\n"
    );
    assert_eq!(topics[2].text, "# Run it\nAgain.\n");
    assert_eq!(topics[3].text, "# ???\n");
}

#[test]
fn a_guide_without_preamble_has_no_empty_overview() {
    assert_eq!(names(&topics("\n## Only\nBody\n")), ["only.md"]);
}

#[test]
fn publication_is_idempotent_per_revision_and_leaves_other_revisions() {
    let state = tempfile::tempdir().unwrap();
    let parent = state.path().join("docs");
    let first = materialize(&parent, &topics("## One\nA\n")).unwrap();
    assert_eq!(materialize(&parent, &topics("## One\nA\n")).unwrap(), first);
    let second = materialize(&parent, &topics("## One\nB\n")).unwrap();
    assert_ne!(first, second);
    assert_eq!(
        std::fs::read_to_string(first.join("one.md")).unwrap(),
        "# One\nA\n"
    );
    assert_eq!(
        std::fs::read_to_string(second.join("one.md")).unwrap(),
        "# One\nB\n"
    );
    let entries = std::fs::read_dir(&parent).unwrap().count();
    assert_eq!(entries, 2, "No staging directory may remain");
}

#[tokio::test]
async fn every_section_of_the_shipped_guide_is_found_first_by_its_title() {
    let state = tempfile::tempdir().unwrap();
    let backend = guide(state.path()).unwrap();
    let cancel = CancellationToken::new();
    let shipped = topics(GUIDE);
    let list = DocsListTool::new(backend.clone())
        .call(serde_json::from_str(r#"{"limit":100}"#).unwrap(), &cancel)
        .await
        .unwrap();
    assert_eq!(list.data.total_documents, shipped.len());
    assert!(shipped.len() > 10, "The guide must be split by section");
    let search = DocsSearchTool::new(backend.clone());
    let read = DocsReadTool::new(backend);
    for document in &list.data.documents {
        assert_eq!(document.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
        let input: DocsSearchInput =
            serde_json::from_value(serde_json::json!({ "query": document.title })).unwrap();
        let found = search.call(input, &cancel).await.unwrap();
        assert_eq!(
            found.data.matches[0].document.document_id, document.document_id,
            "Searching {:?}",
            document.title
        );
        let input: DocsReadInput = serde_json::from_value(
            serde_json::json!({ "document_id": document.document_id, "limit": 500 }),
        )
        .unwrap();
        let page = read.call(input, &cancel).await.unwrap();
        assert!(
            !page.data.truncated,
            "{} must fit one read page",
            document.document_id
        );
    }
}

/// The system prompt sends an agent to this document by its id.
#[test]
fn the_document_the_prompt_names_is_in_the_guide() {
    assert!(names(&topics(GUIDE)).contains(&"skills.md"));
    assert!(include_str!("../agent/coding-agent.txt").contains("document_id skills.md"));
}
