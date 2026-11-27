//! Explicit application configuration; repository files are never auto-loaded.

use std::path::PathBuf;

use roomkit_tui::UiOptions;

use crate::{Result, permissions::Mode};

mod appearance;
mod cli;
mod file;

use appearance::ui_options;
pub(crate) use cli::{Cli, Color, Palette};
pub(crate) use file::{DEFAULT_TOOL_ROUNDS, UNATTENDED_TOOL_ROUNDS};
use file::{FileConfig, Pricing, Provider, ShellOptions};

pub(crate) struct Settings {
    pub memory: crate::project_memory::Config,
    pub local_only: bool,
    pub executor_url: Option<String>,
    pub serve_pool: Option<String>,
    pub executor_pool: Option<String>,
    pub remote_worker: Option<String>,
    pub update_check: bool,
    pub auto_title: bool,
    pub require_tool_intent: bool,
    pub herdr: bool,
    pub resume_options: Option<Vec<String>>,
    pub workspace: PathBuf,
    pub state_dir: PathBuf,
    /// Neither the flag nor its variable named the storage.
    pub default_storage: bool,
    pub session: Option<String>,
    pub session_group: Option<String>,
    pub provider: crate::providers::ProviderKind,
    pub base_url: String,
    pub model: String,
    pub provider_timeout_seconds: u64,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u32>,
    pub thinking_budget: Option<u32>,
    /// The reasoning effort a project's first agent starts with.
    pub reasoning_effort: Option<String>,
    /// The list price `[provider.pricing]` states for `model` on `base_url`.
    pub pricing: Option<roomkit_ai::ModelPricing>,
    pub debug: bool,
    pub ui: UiOptions,
    pub mode: Mode,
    /// Whether the launch named its mode: a conversation then opens in it,
    /// not in the mode it was left in.
    pub mode_given: bool,
    /// Host variables commands receive beyond the development list.
    pub inherit_env: Vec<String>,
    /// Rounds of tool calls one turn may run.
    pub max_tool_rounds: u32,
    /// Nobody watches this launch (`--prompt`): no person can answer an
    /// approval, so agents get no tool that waits for one.
    pub unattended: bool,
}

impl Settings {
    /// How the launch options' connection authenticates when it starts a
    /// project: with the environment variable that holds a key now, read
    /// again at every launch, never copied to the profile, so a rotated key
    /// is the one used. Saved profiles remain authoritative thereafter.
    pub fn initial_authentication(&self) -> crate::profiles::model::Authentication {
        use crate::profiles::model::Authentication;
        if self.provider == crate::providers::ProviderKind::LlamaCpp {
            return Authentication::None;
        }
        let mut variables = vec!["ROOMKIT_API_KEY"];
        if self.provider == crate::providers::ProviderKind::Anthropic {
            variables.push("ANTHROPIC_API_KEY");
        }
        variables
            .into_iter()
            .find(|name| std::env::var(name).is_ok_and(|key| !key.is_empty()))
            .map_or(Authentication::None, |variable| {
                Authentication::Environment {
                    variable: variable.into(),
                }
            })
    }

    pub fn load(cli: &Cli) -> Result<Self> {
        let file = read_file(cli)?;
        if cli
            .session
            .as_deref()
            .is_some_and(|session| !valid_session_name(session))
        {
            return Err(
                "Session names use 1–64 ASCII letters, digits, hyphens or underscores".into(),
            );
        }
        let first = FirstAgent::read(cli, file.provider)?;
        let max_tool_rounds = tool_rounds(
            cli.max_tool_rounds.or(file.agent.max_tool_rounds),
            cli.prompt.is_some(),
        )?;
        let (workspace, state_dir) = locations(cli)?;
        Ok(Self {
            auto_title: !cli.no_auto_title && cli.prompt.is_none(),
            memory: file.memory.checked()?,
            update_check: file.updates.check && !cli.no_update_check,
            require_tool_intent: cli
                .require_tool_intent
                .unwrap_or(file.agent.require_tool_intent),
            herdr: file.integrations.herdr && !cli.no_herdr,
            inherit_env: inherit_env(cli, file.shell)?,
            resume_options: resume_options(cli)?,
            local_only: cli.local,
            executor_url: None,
            serve_pool: cli.serve_pool.clone(),
            executor_pool: cli.executor_pool.clone(),
            remote_worker: cli.remote_worker.clone(),
            workspace,
            debug: cli.debug,
            state_dir,
            default_storage: cli.state_dir.is_none(),
            session: cli.session.clone(),
            session_group: cli.session_group.clone(),
            mode: cli.mode.unwrap_or(file.permissions.mode),
            mode_given: cli.mode.is_some(),
            provider: first.provider,
            base_url: first.base_url,
            model: first.model,
            provider_timeout_seconds: first.timeout_seconds,
            context_window: first.context_window,
            max_output_tokens: first.max_output_tokens,
            thinking_budget: first.thinking_budget,
            reasoning_effort: first.reasoning_effort,
            pricing: first.pricing,
            ui: ui_options(cli, file.ui)?,
            max_tool_rounds,
            unattended: cli.prompt.is_some(),
        })
    }
}

/// The `--config` file as it reads, or the defaults without one.
fn read_file(cli: &Cli) -> Result<FileConfig> {
    let Some(path) = &cli.config else {
        return Ok(FileConfig::default());
    };
    let text = std::fs::read_to_string(path)
        .map_err(|_| "Cannot read --config; choose an existing UTF-8 TOML file")?;
    // Do not echo configuration lines that might contain credentials, nor
    // the parser's message, which quotes values: the line it stopped at
    // says where to look.
    Ok(
        toml::from_str::<FileConfig>(&text).map_err(|error| match error.span() {
            Some(span) => format!(
                "Invalid configuration at line {}; see roomkit-code.example.toml",
                text[..span.start.min(text.len())].matches('\n').count() + 1
            ),
            None => "Invalid configuration; see roomkit-code.example.toml".to_owned(),
        })?,
    )
}

/// What the options and the file say of the project's first agent: its
/// connection and its model, checked.
struct FirstAgent {
    provider: crate::providers::ProviderKind,
    base_url: String,
    model: String,
    timeout_seconds: u64,
    context_window: Option<u64>,
    max_output_tokens: Option<u32>,
    thinking_budget: Option<u32>,
    reasoning_effort: Option<String>,
    pricing: Option<roomkit_ai::ModelPricing>,
}

impl FirstAgent {
    /// The options first, then `file`.
    fn read(cli: &Cli, file: Provider) -> Result<Self> {
        let provider = cli.provider.or(file.kind).unwrap_or_default();
        let given = |option: &Option<String>, file: Option<String>| {
            let value = option.clone().or(file);
            value
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_default()
        };
        let mut base_url = given(&cli.base_url, file.base_url);
        if !base_url.is_empty() {
            check_base_url(&base_url)?;
        }
        let model = given(&cli.model, file.model);
        if base_url.is_empty() && !model.is_empty() {
            base_url = provider.default_url().into();
        }
        if provider == crate::providers::ProviderKind::LlamaCpp {
            if !base_url.is_empty() {
                return Err("Managed llama.cpp chooses its own local address".into());
            }
        } else if base_url.is_empty() != model.is_empty() {
            return Err(
                "Supply both a base URL and model, or configure them with /providers and /agents"
                    .into(),
            );
        }
        let timeout_seconds = cli
            .provider_timeout_seconds
            .or(file.timeout_seconds)
            .unwrap_or(600);
        if !(1..=86_400).contains(&timeout_seconds) {
            return Err("provider timeout must be between 1 and 86400 seconds".into());
        }
        let context_window = cli.context_window.or(file.context_window);
        if context_window == Some(0) {
            return Err("context window must be a positive token count".into());
        }
        let reasoning_effort = cli.reasoning_effort.clone().or(file.reasoning_effort);
        if reasoning_effort
            .as_deref()
            .is_some_and(|effort| !crate::profiles::model::EFFORTS.contains(&effort))
        {
            return Err(format!(
                "reasoning effort must be one of {}",
                crate::profiles::model::EFFORTS.join(", ")
            )
            .into());
        }
        let max_output_tokens = cli.max_output_tokens.or(file.max_output_tokens);
        if max_output_tokens == Some(0) {
            return Err("max output tokens must be a positive token count".into());
        }
        let thinking_budget = cli.thinking_budget.or(file.thinking_budget);
        crate::local_models::validate_thinking_budget(
            provider,
            thinking_budget,
            max_output_tokens,
        )?;
        Ok(Self {
            thinking_budget,
            provider,
            base_url,
            model,
            timeout_seconds,
            context_window,
            max_output_tokens,
            reasoning_effort,
            pricing: file.pricing.map(Pricing::read).transpose()?,
        })
    }
}

/// Refuses a base URL that is not an API root: its credential goes in
/// `ROOMKIT_API_KEY`, never in the URL.
fn check_base_url(base_url: &str) -> Result<()> {
    let url = url::Url::parse(base_url).map_err(|_| "Invalid provider base URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Use an HTTP(S) API root without credentials, query or fragment; set ROOMKIT_API_KEY separately".into());
    }
    Ok(())
}

/// The project's directory, which must exist, and where the application
/// keeps its state.
fn locations(cli: &Cli) -> Result<(PathBuf, PathBuf)> {
    let workspace = cli
        .workspace
        .canonicalize()
        .map_err(|_| format!("Workspace does not exist: {}", cli.workspace.display()))?;
    if !workspace.is_dir() {
        return Err("Workspace must be a directory".into());
    }
    let state_dir = cli
        .state_dir
        .clone()
        .or_else(default_state_dir)
        .ok_or("Cannot locate application storage; supply --state-dir")?;
    Ok((workspace, std::path::absolute(state_dir)?))
}

/// The rounds of tool calls a turn may run: at least one when set, else
/// the default of an attended or an unattended launch.
fn tool_rounds(rounds: Option<u32>, unattended: bool) -> Result<u32> {
    match rounds {
        Some(0) => Err("max tool rounds must be at least 1".into()),
        Some(rounds) => Ok(rounds),
        None if unattended => Ok(UNATTENDED_TOOL_ROUNDS),
        None => Ok(DEFAULT_TOOL_ROUNDS),
    }
}

pub(crate) fn valid_session_name(session: &str) -> bool {
    !session.is_empty()
        && session.len() <= 64
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}

pub(crate) fn default_state_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    if cfg!(target_os = "macos") {
        home.map(|p| p.join("Library/Application Support/roomkit-code"))
    } else if cfg!(target_os = "linux") {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.map(|p| p.join(".local/share")))
            .map(|p| p.join("roomkit-code"))
    } else {
        None
    }
}

/// The added shell variables: `--inherit-env`, else ROOMKIT_INHERIT_ENV,
/// else the configuration file's. The variable is read here rather than by
/// clap, so a resume command replays only the flag: a resumed session reads
/// the variable anew, as direnv sets it.
fn inherit_env(cli: &Cli, file: ShellOptions) -> Result<Vec<String>> {
    choose_inherit_env(
        &cli.inherit_env,
        std::env::var("ROOMKIT_INHERIT_ENV").ok(),
        file.inherit_env,
    )
}

/// Every source is checked, even one a higher source replaces; empty entries
/// (an empty variable, a trailing comma) are dropped.
fn choose_inherit_env(
    flag: &[String],
    variable: Option<String>,
    file: Vec<String>,
) -> Result<Vec<String>> {
    let clean = |names: Vec<String>| -> Result<Vec<String>> {
        let names: Vec<String> = names
            .iter()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .collect();
        crate::command_env::validate(&names)?;
        Ok(names)
    };
    let flag = clean(flag.to_vec())?;
    let variable = clean(
        variable
            .unwrap_or_default()
            .split(',')
            .map(str::to_owned)
            .collect(),
    )?;
    let file = clean(file)?;
    Ok([flag, variable, file]
        .into_iter()
        .find(|names| !names.is_empty())
        .unwrap_or_default())
}

/// Retain explicit UI/configuration choices; provider secrets and bootstrap
/// options are deliberately absent. Persisted profiles restore agent/model.
fn resume_options(cli: &Cli) -> Result<Option<Vec<String>>> {
    let mut args = vec![];
    if let Some(group) = &cli.executor_pool {
        args.extend(["--executor-pool".into(), group.clone()]);
    }
    if let Some(group) = &cli.remote_worker {
        args.extend(["--remote-worker".into(), group.clone()]);
    }
    if !cli.inherit_env.is_empty() {
        args.extend(["--inherit-env".into(), cli.inherit_env.join(",")]);
    }
    if let Some(required) = cli.require_tool_intent {
        args.push(format!("--require-tool-intent={required}"));
    }
    if let Some(rounds) = cli.max_tool_rounds {
        args.extend(["--max-tool-rounds".into(), rounds.to_string()]);
    }
    if let Some(path) = &cli.config {
        let path = std::path::absolute(path)?;
        // Non-UTF-8 paths disable resume without preventing application launch.
        let Some(path) = path.to_str() else {
            return Ok(None);
        };
        args.extend(["--config".into(), path.into()]);
    }
    if let Some(theme) = cli.theme {
        args.extend([
            "--theme".into(),
            match theme {
                Palette::Dark => "dark",
                Palette::Light => "light",
                Palette::Terminal => "terminal",
            }
            .into(),
        ]);
    }
    if let Some(color) = cli.color {
        args.extend([
            "--color".into(),
            match color {
                Color::Auto => "auto",
                Color::Always => "always",
                Color::Never => "never",
            }
            .into(),
        ]);
    }
    for (enabled, flag) in [
        (cli.local, "--local"),
        (cli.no_animations, "--no-animations"),
        (cli.native_selection, "--native-selection"),
        (cli.no_logo, "--no-logo"),
        (cli.no_terminal_status, "--no-terminal-status"),
        (cli.no_herdr, "--no-herdr"),
        (cli.no_update_check, "--no-update-check"),
        (cli.no_auto_title, "--no-auto-title"),
        (cli.debug, "--debug"),
    ] {
        if enabled {
            args.push(flag.into());
        }
    }
    Ok(Some(args))
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
