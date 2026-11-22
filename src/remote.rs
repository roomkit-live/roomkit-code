//! Remote delegation using RoomKit's Redis turn transport.

pub(crate) mod pool;

use std::sync::Arc;

use roomkit::turns::TurnBackendHandle;
use roomkit::{RedisTurnBackend, RedisTurnBackendOptions, TurnExecutor, TurnExecutorOptions};

use crate::{Result, config::Settings, profiles::Profiles, storage::Storage};

pub(crate) const AGENT: &str = "remote-linux";
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

fn address() -> Result<String> {
    let address = std::env::var("ROOMKIT_EXECUTOR_REDIS_URL")
        .map_err(|_| "Set ROOMKIT_EXECUTOR_REDIS_URL for this executor group")?;
    let url = url::Url::parse(&address).map_err(|_| "Invalid executor Redis URL")?;
    if !matches!(url.scheme(), "redis" | "rediss") || url.host().is_none() {
        return Err("Use a redis:// or rediss:// executor URL with a host".into());
    }
    Ok(address)
}

fn backend(group: &str) -> Result<RedisTurnBackend> {
    Ok(RedisTurnBackend::new(
        Some(&address()?),
        RedisTurnBackendOptions {
            key_prefix: format!("roomkit:code:{group}"),
            ..Default::default()
        },
    )?)
}

pub(crate) fn handle(group: &str) -> Result<TurnBackendHandle> {
    Ok(TurnBackendHandle::new(Arc::new(backend(group)?)))
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
    let prefix = if args.is_some() {
        format!("pool:{group}")
    } else {
        group.into()
    };
    let backend = backend(&prefix)?;
    // The worker serves no delegation or human-input tools of its own.
    settings.unattended = true;
    let storage = Storage::open(&settings.state_dir, &settings.workspace)?;
    let profiles = Profiles::load(&settings, &storage, settings.initial_api_key(), None).await?;
    let served = run(&profiles, &backend, group, args).await;
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
    executor.hook(
        roomkit::HookTrigger::BeforeToolUse,
        move |event: roomkit::ToolCallEvent, _| async move { authorize(event, pooled).await },
        roomkit::turns::HookOptions {
            name: "remote-workspace-permissions".into(),
            fail_closed: true,
            timeout: crate::timeouts::PERMISSION_GATE,
            ..Default::default()
        },
    )?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    if args.is_some() {
        pool::declare(&executor)?;
    }
    backend.serve(executor.clone()).await?;
    let mut presence = match args {
        Some(args) => {
            let inventory = profiles
                .executor_inventory
                .as_ref()
                .ok_or("Missing executor pool inventory")?;
            Some(pool::advertise(inventory, args, offered, &executor).await?)
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
        error = pool::worker_failure(&mut presence) => Err(std::io::Error::other(error)),
    };
    if let Some(p) = presence {
        p.retire().await;
    }
    executor.close().await;
    Ok(stopped?)
}

pub(crate) fn permits(tool: &str, pooled: bool) -> bool {
    TOOLS.contains(&tool) || (pooled && matches!(tool, pool::LIST_EXECUTORS | pool::CARGO_TEST))
}

async fn authorize(event: roomkit::ToolCallEvent, pooled: bool) -> crate::permissions::Verdict {
    use crate::permissions::{allow, block};
    if !permits(&event.name, pooled) {
        return block("This tool is unavailable on the remote worker");
    }
    let Some(host) = roomkit::current_turn_host() else {
        return block("No room owner is available to authorize this tool");
    };
    let fields = [
        ("name".into(), event.name.into()),
        (
            "arguments".into(),
            serde_json::Value::Object(event.arguments),
        ),
        ("id".into(), event.tool_call_id.into()),
    ]
    .into_iter()
    .collect();
    match host.call(AUTHORIZE, Some(fields)).await {
        Ok(serde_json::Value::Bool(true)) => allow(),
        Ok(serde_json::Value::String(reason)) => block(reason),
        _ => block("The room owner did not authorize this tool"),
    }
}
