#![expect(clippy::unwrap_used, reason = "fixture failures fail the test")]
use super::*;
fn skill(root: &Path, name: &str, body: &str) {
    let directory = root.join(name);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Fixture skill\n---\n{body}"),
    )
    .unwrap();
}
#[tokio::test]
async fn roots_have_deterministic_precedence_and_bad_skills_do_not_hide_valid_ones() {
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    skill(&state.path().join("skills"), "review", "User version");
    skill(
        &workspace.path().join(".agents/skills"),
        "review",
        "Project version",
    );
    skill(
        &workspace.path().join(".roomkit/skills"),
        "review",
        "Native version",
    );
    skill(&workspace.path().join(".agents/skills"), "valid", "Good");
    std::fs::create_dir_all(workspace.path().join(".agents/skills/broken")).unwrap();
    let catalogue = Catalogue::load(workspace.path().into(), state.path().into())
        .await
        .unwrap();
    assert_eq!(
        catalogue
            .selected(&["review".into()])
            .unwrap()
            .get_skill("review")
            .unwrap()
            .unwrap()
            .instructions(),
        "Native version"
    );
    assert!(catalogue.entries["broken"].error.is_some());
    assert_eq!(
        catalogue
            .selected(&["valid".into(), "broken".into()])
            .unwrap()
            .skill_count(),
        1
    );
    // A broken override must not silently reactivate an earlier version.
    std::fs::write(
        workspace.path().join(".roomkit/skills/review/SKILL.md"),
        "invalid",
    )
    .unwrap();
    let catalogue = Catalogue::load(workspace.path().into(), state.path().into())
        .await
        .unwrap();
    assert!(
        !catalogue
            .selected(&["review".into()])
            .unwrap()
            .has_entries()
    );
}
#[cfg(unix)]
#[tokio::test]
async fn discovery_refuses_escape_links_and_foreign_invocation_sidecars() {
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    skill(outside.path(), "linked", "Outside");
    let root = workspace.path().join(".agents/skills");
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(outside.path().join("linked"), root.join("linked")).unwrap();
    skill(&root, "policy", "Instructions");
    std::fs::create_dir_all(root.join("policy/agents")).unwrap();
    std::fs::write(
        root.join("policy/agents/openai.yaml"),
        "policy:\n  allow_implicit_invocation: false\n",
    )
    .unwrap();
    let catalogue = Catalogue::load(workspace.path().into(), state.path().into())
        .await
        .unwrap();
    assert!(
        catalogue.entries["linked"]
            .error
            .as_ref()
            .unwrap()
            .contains("escapes")
    );
    assert!(catalogue.entries["policy"].error.is_some());
    assert!(
        !catalogue
            .selected(&["linked".into(), "policy".into()])
            .unwrap()
            .has_entries()
    );
}

/// Another assistant's invocation field restricts who starts the skill,
/// and nothing here enforces it: the skill is listed with the field as
/// its loading error and reaches no agent, where loading it would drop
/// the restriction without a word. The field refuses whatever its value,
/// one the engine's metadata leaves out included.
#[tokio::test]
async fn a_skill_declaring_another_assistant_s_invocation_field_is_refused() {
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let root = workspace.path().join(".agents/skills");
    let declared = [
        ("manual", "disable-model-invocation", " true"),
        ("hidden", "user-invocable", " false"),
        ("forked", "context", " fork"),
        ("routed", "agent", " Explore"),
        ("bare", "agent", ""),
        ("nested", "agent", "\n  name: Explore"),
    ];
    for (name, field, value) in declared {
        let directory = root.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Fixture skill\n{field}:{value}\n---\nBody"),
        )
        .unwrap();
    }
    // The same words in the instructions declare nothing.
    skill(&root, "plain", "agent: Explore");
    let catalogue = Catalogue::load(workspace.path().into(), state.path().into())
        .await
        .unwrap();
    for (name, field, _) in declared {
        let error = catalogue.entries[name].error.as_deref();
        assert!(
            error.is_some_and(|error| error.contains(field)),
            "{name}: {error:?}"
        );
    }
    let mut names: Vec<String> = declared.iter().map(|skill| skill.0.to_owned()).collect();
    names.push("plain".to_owned());
    let selected = catalogue.selected(&names).unwrap();
    assert_eq!(selected.skill_count(), 1);
    assert!(selected.get_skill("plain").unwrap().is_some());
}
