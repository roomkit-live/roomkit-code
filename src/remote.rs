//! Remote delegation using RoomKit's Redis turn transport.

mod outcomes;
pub(crate) mod pool;
pub(crate) use outcomes::{OUTCOME, REPORT_LIFETIME, ToolOutcome};

use std::sync::Arc;

use roomkit::turns::TurnBackendHandle;
use roomkit::{RedisTurnBackend, RedisTurnBackendOptions, TurnExecutor, TurnExecutorOptions};

use crate::{Result, config::Settings, profiles::Profiles, storage::Storage};

pub(crate) use crate::names::REMOTE_AGENT as AGENT;
pub(crate) const LABEL: &str = "Remote worker";
pub(crate) const TOOLS: &[&str] = &[
    "list",
    "glob",
    "grep",
    "read",
    "read_stored_result",
    "code_outline",
    "diff",
    "edit",
    "write",
    "patch",
];
pub(crate) const AUTHORIZE: &str = "code_authorize_workspace_tool";

pub(crate) fn group(value: &str) -> std::result::Result<String, String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err("Use 1–64 letters, digits, hyphens or underscores for the Redis group".into());
    }
    Ok(value.into())
}

pub(crate) fn address(saved: Option<&str>) -> Result<String> {
    let address = match std::env::var("ROOMKIT_EXECUTOR_REDIS_URL") {
        Ok(endpoint) => endpoint,
        Err(std::env::VarError::NotPresent) => saved
            .ok_or("Set ROOMKIT_EXECUTOR_REDIS_URL or configure a Redis connection in /executors")?
            .to_owned(),
        Err(_) => return Err("Invalid ROOMKIT_EXECUTOR_REDIS_URL".into()),
    };
    validate_address(&address)?;
    Ok(address)
}

fn validate_address(address: &str) -> std::result::Result<(), String> {
    let url = url::Url::parse(address).map_err(|_| "Invalid executor Redis URL")?;
    if !matches!(url.scheme(), "redis" | "rediss") || url.host().is_none() {
        return Err("Use a redis:// or rediss:// executor URL with a host".into());
    }
    redis::Client::open(address).map_err(|_| "Invalid executor Redis URL")?;
    Ok(())
}

fn backend(group: &str, address: &str) -> Result<RedisTurnBackend> {
    Ok(RedisTurnBackend::new(
        Some(address),
        RedisTurnBackendOptions {
            key_prefix: format!("roomkit:code:{group}"),
            ..Default::default()
        },
    )?)
}

pub(crate) fn handle(group: &str) -> Result<TurnBackendHandle> {
    Ok(TurnBackendHandle::new(Arc::new(backend(
        group,
        &address(None)?,
    )?)))
}

pub(crate) async fn serve(
    mut settings: Settings,
    group: &str,
    args: Option<&pool::WorkerArgs>,
) -> Result<()> {
    match args {
        Some(_) if !cfg!(any(target_os = "linux", target_os = "macos")) => {
            return Err("Executor pool workers require Linux or macOS".into());
        }
        None if !cfg!(target_os = "linux") => {
            return Err("The single-worker executor requires Linux".into());
        }
        _ => {}
    }
    // The worker serves no delegation or human-input tools of its own.
    settings.unattended = true;
    let storage = Storage::open(&settings.state_dir, &settings.workspace)?;
    let profiles =
        Profiles::load(&settings, &storage, settings.initial_authentication(), None).await?;
    // A worker has no terminal to hide a panic's message, and no one
    // watching it either: its record goes to the diagnostic log.
    crate::diagnostics::report_panics(profiles.diagnostics.clone());
    let backend = match &profiles.executor_inventory {
        Some(inventory) => inventory.backend.clone(),
        None => backend(group, &address(settings.executor_url.as_deref())?)?,
    };
    let served = run(&profiles, &backend, group, args, &settings.workspace).await;
    let closed = backend.close().await;
    profiles.resources.compactions.close().await;
    profiles.accounts.close().await;
    profiles.mcp_runtime.close().await;
    let cleaned = profiles.resources.close(false).await;
    profiles.diagnostics.close().await;
    served?;
    closed?;
    if !cleaned {
        return Err("Worker workspace cleanup is still pending".into());
    }
    Ok(())
}

async fn run(
    profiles: &Profiles,
    backend: &RedisTurnBackend,
    group: &str,
    args: Option<&pool::WorkerArgs>,
    workspace: &std::path::Path,
) -> Result<()> {
    let requested = args
        .map(|a| a.worker_profile.as_slice())
        .unwrap_or_default();
    let (agent, offered) = profiles.worker(requested).await?;
    let pooled = args.is_some();
    let executor = TurnExecutor::new(
        move |id| std::future::ready(Ok((id.as_str() == AGENT).then(|| agent.clone()))),
        TurnExecutorOptions {
            hosts: Some([AGENT.to_owned()].into()),
            executor_id: args.map(pool::WorkerArgs::instance).transpose()?,
            ..Default::default()
        },
    )?;
    let reports = outcomes::install(&executor)?;
    executor.hook(
        roomkit::HookTrigger::BeforeToolUse,
        move |event: roomkit::ToolCallEvent, _| {
            let reports = reports.clone();
            async move { authorize(event, pooled, &reports).await }
        },
        roomkit::turns::HookOptions {
            name: "remote-workspace-permissions".into(),
            fail_closed: true,
            timeout: crate::timeouts::PERMISSION_GATE,
            ..Default::default()
        },
    )?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    backend.serve(executor.clone()).await?;
    let mut presence = match args {
        Some(args) => {
            let inventory = profiles
                .executor_inventory
                .as_ref()
                .ok_or("Missing executor pool inventory")?;
            Some(pool::advertise(inventory, args, offered, &executor, workspace).await?)
        }
        None => None,
    };
    println!(
        "Serving {AGENT} in group {group} (executor {}, PID {})",
        executor.executor_id(),
        std::process::id()
    );
    let stopped = tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        _ = terminate.recv() => Ok(()),
        // A worker started from a terminal that closes retires as well.
        _ = hangup.recv() => Ok(()),
        error = pool::worker_failure(&mut presence) => Err(std::io::Error::other(error)),
    };
    if let Some(p) = presence {
        p.retire().await;
    }
    executor.close().await;
    Ok(stopped?)
}

pub(crate) fn permits(tool: &str, pooled: bool) -> bool {
    TOOLS.contains(&tool)
        || (pooled
            && matches!(
                tool,
                pool::LIST_EXECUTORS
                    | pool::CARGO_TEST
                    | pool::ASK_PEER
                    | pool::ANSWER_PEER
                    | crate::names::SEND_MESSAGE
            ))
}

async fn authorize(
    event: roomkit::ToolCallEvent,
    pooled: bool,
    reports: &outcomes::Reports,
) -> crate::permissions::Verdict {
    use crate::permissions::{allow, block};
    if !permits(&event.name, pooled) {
        return block("This tool is unavailable on the remote worker");
    }
    // The owner applies the same permission gate inside its consultation verb,
    // immediately before the exchange. Running it here too would ask twice.
    if matches!(
        event.name.as_str(),
        pool::ASK_PEER | pool::ANSWER_PEER | crate::names::SEND_MESSAGE
    ) {
        return allow();
    }
    let Some(host) = roomkit::current_turn_host() else {
        return block("No room owner is available to authorize this tool");
    };
    let call = event.tool_call_id.clone();
    let mut fields = tool_fields(event);
    fields.insert("report_outcomes".into(), true.into());
    let Ok(mut decision) = host.call(AUTHORIZE, Some(fields)).await else {
        return block("The room owner did not authorize this tool");
    };
    if let Some(receipt) = decision
        .get("report_receipt")
        .and_then(serde_json::Value::as_str)
    {
        reports.request(host.turn_id(), call, receipt.to_owned());
        decision = decision["decision"].take();
    }
    match decision {
        serde_json::Value::Bool(true) => allow(),
        serde_json::Value::String(reason) => block(reason),
        _ => block("The room owner did not authorize this tool"),
    }
}

fn tool_fields(event: roomkit::ToolCallEvent) -> serde_json::Map<String, serde_json::Value> {
    [
        ("name".into(), event.name.into()),
        (
            "arguments".into(),
            serde_json::Value::Object(event.arguments),
        ),
        ("id".into(), event.tool_call_id.into()),
    ]
    .into_iter()
    .collect()
}
