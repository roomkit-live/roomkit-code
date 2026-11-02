//! The profile files a project opens with: read under the load lease,
//! validated, migrated to the current schema, and given a first agent when a
//! configured connection starts a new project.

use super::*;

pub(super) struct Documents {
    pub mcp: Document<mcp::Servers>,
    pub providers: Document<Providers>,
    pub project: Document<Project>,
    pub global: Document<Project>,
}

pub(super) async fn load(
    settings: &Settings,
    storage: &Storage,
    api_key: String,
) -> Result<Documents, String> {
    let _lease = store::load_lease(&settings.state_dir).await?;
    let mcp = Document::<mcp::Servers>::load(storage.database.with_file_name("mcp.json"))?;
    mcp.value.validate()?;
    let mut documents = Documents {
        mcp,
        providers: Document::<Providers>::load(settings.state_dir.join("connections.json"))?,
        project: Document::<Project>::load(storage.database.with_file_name("agents.json"))?,
        global: Document::<Project>::load(settings.state_dir.join("agents.json"))?,
    };
    validate(&documents)?;
    for document in [&mut documents.project, &mut documents.global] {
        let mut migrated = document.value.clone();
        if migrated.migrate()? {
            document.save(migrated)?;
        }
    }
    if starts_a_new_project(&documents, settings) {
        add_first_agent(&mut documents, settings, api_key)?;
    }
    Ok(documents)
}

fn validate(documents: &Documents) -> Result<(), String> {
    let Documents {
        providers,
        project,
        global,
        ..
    } = documents;
    if providers.value.schema != 1 {
        return Err("Unsupported profile file version".into());
    }
    if providers.value.profiles.len() > 128
        || project.value.agents.len() > catalog::MAX_AGENTS
        || global.value.agents.len() > catalog::MAX_AGENTS
    {
        return Err("Too many provider or agent profiles".into());
    }
    for (id, provider) in &providers.value.profiles {
        provider.validate()?;
        if id != &provider.id {
            return Err("Invalid provider identity".into());
        }
    }
    for (id, agent) in project.value.agents.iter().chain(&global.value.agents) {
        agent.validate()?;
        if id != &agent.id {
            return Err("Invalid agent identity".into());
        }
    }
    Ok(())
}

/// A configured connection starts a new project with its own agent, unless a
/// global agent already lets it start.
fn starts_a_new_project(documents: &Documents, settings: &Settings) -> bool {
    documents.project.revision().is_none()
        && documents.project.value.agents.is_empty()
        && documents.global.value.agents.is_empty()
        && !settings.base_url.is_empty()
}

/// The launch options' connection, saved once per workspace, and an agent
/// with every tool on it.
fn add_first_agent(
    documents: &mut Documents,
    settings: &Settings,
    api_key: String,
) -> Result<(), String> {
    let digest = crate::digest::sha256_hex(settings.workspace.as_os_str().as_encoded_bytes());
    let id = format!("imported-{}", &digest[..12]);
    if !documents.providers.value.profiles.contains_key(&id) {
        let provider = Provider {
            id: id.clone(),
            label: "Imported connection".into(),
            base_url: settings.base_url.clone(),
            authentication: if api_key.is_empty() {
                Authentication::None
            } else {
                Authentication::Stored { key: api_key }
            },
            timeout_seconds: settings.provider_timeout_seconds,
        };
        provider.validate()?;
        let mut updated = documents.providers.value.clone();
        updated.profiles.insert(id.clone(), provider);
        documents.providers.save(updated)?;
    }
    let agent = Agent {
        id: "code".into(),
        label: "Code".into(),
        provider: id,
        model: settings.model.clone(),
        reasoning_effort: None,
        vision: false,
        skills: Vec::new(),
        mcp: BTreeMap::new(),
        context_window: settings.context_window,
        max_output_tokens: settings.max_output_tokens,
        instructions: String::new(),
        tools: TOOLS.iter().map(|s| (*s).into()).collect(),
    };
    agent.validate()?;
    let mut updated = documents.project.value.clone();
    updated.selected = Some(agent.id.clone());
    updated.agents.insert(agent.id.clone(), agent);
    documents.project.save(updated)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "Invalid fixtures fail the test")]
    use super::*;
    use clap::Parser;

    fn settings(workspace: &std::path::Path, state: &std::path::Path, base: &str) -> Settings {
        let cli = crate::config::Cli::try_parse_from([
            "roomkit-code",
            workspace.to_str().unwrap(),
            "--state-dir",
            state.to_str().unwrap(),
            "--base-url",
            base,
            "--model",
            "fixture-model",
        ])
        .unwrap();
        Settings::load(&cli).unwrap()
    }

    fn write_private(path: &std::path::Path, value: &serde_json::Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[tokio::test]
    async fn a_configured_connection_starts_a_new_project_with_its_own_agent() {
        let workspace = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut settings = settings(workspace.path(), state.path(), "http://127.0.0.1:9/v1");
        settings.max_output_tokens = Some(4096);
        let storage = Storage::open(&settings.state_dir, &settings.workspace).unwrap();
        let documents = load(&settings, &storage, String::new()).await.unwrap();
        let agent = &documents.project.value.agents["code"];
        assert_eq!(agent.model, "fixture-model");
        assert_eq!(agent.max_output_tokens, Some(4096));
        assert_eq!(agent.tools.len(), TOOLS.len());
        assert_eq!(documents.project.value.selected.as_deref(), Some("code"));
        assert!(
            documents
                .providers
                .value
                .profiles
                .contains_key(&agent.provider)
        );
    }

    #[tokio::test]
    async fn a_global_agent_lets_a_new_project_start_without_its_own() {
        let workspace = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let settings = settings(workspace.path(), state.path(), "http://127.0.0.1:9/v1");
        let storage = Storage::open(&settings.state_dir, &settings.workspace).unwrap();
        let shared = serde_json::json!({
            "id": "shared", "label": "Shared", "provider": "p", "model": "m",
            "instructions": "", "tools": [],
        });
        write_private(
            &settings.state_dir.join("agents.json"),
            &serde_json::json!({"schema": PROJECT_SCHEMA, "agents": {"shared": shared}}),
        );
        let documents = load(&settings, &storage, String::new()).await.unwrap();
        assert!(documents.project.value.agents.is_empty());
        assert!(documents.providers.value.profiles.is_empty());
    }

    #[tokio::test]
    async fn an_agent_stored_under_another_id_is_refused() {
        let workspace = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let settings = settings(workspace.path(), state.path(), "http://127.0.0.1:9/v1");
        let storage = Storage::open(&settings.state_dir, &settings.workspace).unwrap();
        let agent = serde_json::json!({
            "id": "real", "label": "Real", "provider": "p", "model": "m",
            "instructions": "", "tools": [],
        });
        write_private(
            &settings.state_dir.join("agents.json"),
            &serde_json::json!({"schema": PROJECT_SCHEMA, "agents": {"alias": agent}}),
        );
        let error = load(&settings, &storage, String::new()).await.err();
        assert_eq!(error.as_deref(), Some("Invalid agent identity"));
    }
}
