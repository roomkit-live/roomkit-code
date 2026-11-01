//! Explicit application configuration; repository files are never auto-loaded.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use roomkit_tui::{
    Branding, ColorMode, SelectionMode, TerminalStatus, Theme, UiOptions, WelcomeScreen,
};
use serde::Deserialize;

use crate::{Result, permissions::Mode};

#[derive(Debug, Parser)]
#[command(version, about = "Explore a project with a RoomKit coding assistant")]
pub(crate) struct Cli {
    /// Install this release binary in a managed directory (usually ~/.local/bin).
    #[arg(long, conflicts_with_all = ["self_update", "rollback", "update_check"])]
    pub install_dir: Option<PathBuf>,
    /// Check for and install a newer stable release, with confirmation.
    #[arg(long, conflicts_with_all = ["rollback", "update_check"])]
    pub self_update: bool,
    /// Restore the previous managed binary, with confirmation.
    #[arg(long, conflicts_with = "update_check")]
    pub rollback: bool,
    /// Check releases without opening the UI or installing.
    #[arg(long)]
    pub update_check: bool,
    /// Confirm an explicit --self-update or --rollback operation.
    #[arg(long)]
    pub yes: bool,
    /// Disable automatic update checks (manual /update remains available).
    #[arg(long, env = "ROOMKIT_NO_UPDATE_CHECK")]
    pub no_update_check: bool,
    /// Project directory used by the coding tools.
    #[arg(default_value = ".")]
    pub workspace: PathBuf,
    /// Explicit TOML configuration file (never loaded from the project implicitly).
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Initial API root for a project without agent profiles.
    #[arg(long, env = "ROOMKIT_BASE_URL", hide_env_values = true)]
    pub base_url: Option<String>,
    /// Initial model for a project without agent profiles.
    #[arg(long, env = "ROOMKIT_MODEL", hide_env_values = true)]
    pub model: Option<String>,
    /// Initial model-request timeout, including its streamed response.
    #[arg(long)]
    pub provider_timeout_seconds: Option<u64>,
    /// Initial effective model context capacity in tokens.
    #[arg(long)]
    pub context_window: Option<u64>,
    /// Open or create a named conversation; without this, start a new session.
    #[arg(long)]
    pub session: Option<String>,
    /// Permission mode: ask approves effects, plan explores, yolo runs directly.
    #[arg(long, value_enum)]
    pub mode: Option<Mode>,
    /// Storage directory; defaults to the user's application data directory.
    #[arg(long, env = "ROOMKIT_CODE_STATE_DIR", hide_env_values = true)]
    pub state_dir: Option<PathBuf>,
    /// Record additional framework lifecycle events in the private diagnostic log.
    #[arg(long)]
    pub debug: bool,
    /// Require the public intent field in tool schemas sent to the model.
    #[arg(long, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    pub require_tool_intent: Option<bool>,
    /// Check launch options and workspace without loading profiles or connecting.
    #[arg(long)]
    pub check_config: bool,
    /// Terminal palette.
    #[arg(long, value_enum)]
    pub theme: Option<Palette>,
    /// Color policy; auto honors NO_COLOR and TERM=dumb.
    #[arg(long, value_enum)]
    pub color: Option<Color>,
    /// Disable activity animation.
    #[arg(long)]
    pub no_animations: bool,
    /// Let the terminal handle selection; disables application mouse clicks.
    #[arg(long)]
    pub native_selection: bool,
    /// Hide the startup logo.
    #[arg(long)]
    pub no_logo: bool,
    /// Disable OSC 7501 program status in the terminal.
    #[arg(long)]
    pub no_terminal_status: bool,
    /// Disable the optional Herdr TUI integration.
    #[arg(long)]
    pub no_herdr: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Palette {
    #[default]
    Dark,
    Light,
    Terminal,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Color {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileConfig {
    agent: AgentOptions,
    provider: Provider,
    ui: Appearance,
    permissions: Permissions,
    integrations: Integrations,
    updates: UpdateOptions,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct UpdateOptions {
    check: bool,
}
impl Default for UpdateOptions {
    fn default() -> Self {
        Self { check: true }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AgentOptions {
    require_tool_intent: bool,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            require_tool_intent: true,
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Integrations {
    herdr: bool,
}

impl Default for Integrations {
    fn default() -> Self {
        Self { herdr: true }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Permissions {
    mode: Mode,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Provider {
    base_url: Option<String>,
    model: Option<String>,
    timeout_seconds: Option<u64>,
    context_window: Option<u64>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Appearance {
    welcome: bool,
    welcome_width: u16,
    welcome_logo: Option<Vec<String>>,
    welcome_wordmark: Option<Vec<String>>,
    bottom_margin: u16,
    sidebar: bool,
    sidebar_width: u16,
    terminal_status: bool,
    theme: Palette,
    color: Color,
    animations: bool,
    native_selection: bool,
    name: String,
    logo: Option<Vec<String>>,
    markdown: bool,
    message_gap: u16,
    highlight_own_messages: bool,
    tool_preview_lines: usize,
    tool_activity: bool,
    input_min_lines: u16,
    input_padding: u16,
    input_placeholder: String,
    input_prompt: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            welcome: true,
            welcome_width: 88,
            welcome_logo: None,
            welcome_wordmark: None,
            bottom_margin: 1,
            sidebar: true,
            sidebar_width: 34,
            terminal_status: true,
            theme: Palette::default(),
            color: Color::default(),
            animations: true,
            native_selection: false,
            name: "RoomKit Code".into(),
            logo: None,
            markdown: true,
            message_gap: 1,
            highlight_own_messages: true,
            tool_preview_lines: 4,
            tool_activity: true,
            input_min_lines: 1,
            input_padding: 1,
            input_placeholder: "Ask about this project…".into(),
            input_prompt: "›".into(),
        }
    }
}

pub(crate) struct Settings {
    pub update_check: bool,
    pub require_tool_intent: bool,
    pub herdr: bool,
    pub resume_options: Option<Vec<String>>,
    pub workspace: PathBuf,
    pub state_dir: PathBuf,
    pub session: Option<String>,
    pub base_url: String,
    pub model: String,
    pub provider_timeout_seconds: u64,
    pub context_window: Option<u64>,
    pub debug: bool,
    pub ui: UiOptions,
    pub mode: Mode,
}

impl Settings {
    pub fn load(cli: &Cli) -> Result<Self> {
        let file = match &cli.config {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .map_err(|_| "Cannot read --config; choose an existing UTF-8 TOML file")?;
                // Do not echo configuration lines that might contain credentials.
                toml::from_str::<FileConfig>(&text)
                    .map_err(|_| "Invalid configuration; see roomkit-code.example.toml")?
            }
            None => FileConfig::default(),
        };
        let base_url = cli
            .base_url
            .clone()
            .or(file.provider.base_url)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_default();
        if !base_url.is_empty() {
            let url = url::Url::parse(&base_url).map_err(|_| "Invalid provider base URL")?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err("Use an HTTP(S) API root without credentials, query or fragment; set ROOMKIT_API_KEY separately".into());
            }
        }
        let model = cli
            .model
            .clone()
            .or(file.provider.model)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_default();
        if base_url.is_empty() != model.is_empty() {
            return Err(
                "Supply both a base URL and model, or configure them with /providers and /agents"
                    .into(),
            );
        }
        if cli
            .session
            .as_deref()
            .is_some_and(|session| !valid_session_name(session))
        {
            return Err(
                "Session names use 1–64 ASCII letters, digits, hyphens or underscores".into(),
            );
        }
        let provider_timeout_seconds = cli
            .provider_timeout_seconds
            .or(file.provider.timeout_seconds)
            .unwrap_or(600);
        if !(1..=86_400).contains(&provider_timeout_seconds) {
            return Err("provider timeout must be between 1 and 86400 seconds".into());
        }
        let context_window = cli.context_window.or(file.provider.context_window);
        if context_window == Some(0) {
            return Err("context window must be a positive token count".into());
        }
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
        let state_dir = std::path::absolute(state_dir)?;
        let appearance = file.ui;
        let custom_logo = appearance.logo.is_some();
        let mut branding = Branding::new(appearance.name)
            .with_subtitle("Explore, edit and test your project")
            .with_logo(appearance.logo.unwrap_or_else(crate::branding::header_logo));
        if cli.no_logo {
            branding.logo.clear();
        }
        let welcome = appearance.welcome.then(|| {
            let mut welcome = WelcomeScreen::default();
            welcome.width = appearance.welcome_width;
            welcome.logo = if cli.no_logo {
                Vec::new()
            } else {
                appearance.welcome_logo.unwrap_or_else(|| {
                    if custom_logo {
                        branding.logo.clone()
                    } else {
                        crate::branding::welcome_logo()
                    }
                })
            };
            welcome.wordmark = if cli.no_logo {
                Vec::new()
            } else {
                appearance.welcome_wordmark.unwrap_or_else(|| {
                    if branding.name == "RoomKit Code" {
                        crate::branding::welcome_wordmark()
                    } else {
                        Vec::new()
                    }
                })
            };
            welcome
        });
        // UiOptions is non-exhaustive: start from its defaults, then set each field.
        let mut ui = UiOptions::default();
        ui.welcome = welcome;
        ui.bottom_margin = appearance.bottom_margin;
        ui.sidebar_visible = appearance.sidebar;
        ui.sidebar_width = appearance.sidebar_width;
        ui.terminal_status = (appearance.terminal_status && !cli.no_terminal_status)
            .then(|| TerminalStatus::new("roomkit-code"))
            .transpose()?;
        ui.branding = branding;
        ui.theme = match cli.theme.unwrap_or(appearance.theme) {
            Palette::Dark => Theme::dark(),
            Palette::Light => Theme::light(),
            Palette::Terminal => Theme::terminal(),
        };
        ui.color = match cli.color.unwrap_or(appearance.color) {
            Color::Auto => ColorMode::Auto,
            Color::Always => ColorMode::Always,
            Color::Never => ColorMode::Never,
        };
        ui.animations = appearance.animations && !cli.no_animations;
        ui.markdown = appearance.markdown;
        ui.message_gap = appearance.message_gap;
        ui.highlight_own_messages = appearance.highlight_own_messages;
        ui.tool_preview_lines = appearance.tool_preview_lines;
        ui.tool_activity = appearance.tool_activity;
        ui.input_min_lines = appearance.input_min_lines;
        ui.input_padding = appearance.input_padding;
        ui.input_placeholder = appearance.input_placeholder;
        ui.input_prompt = appearance.input_prompt;
        ui.selection = if cli.native_selection || appearance.native_selection {
            SelectionMode::Native
        } else {
            SelectionMode::CopyOnSelect
        };
        Ok(Self {
            update_check: file.updates.check && !cli.no_update_check,
            require_tool_intent: cli
                .require_tool_intent
                .unwrap_or(file.agent.require_tool_intent),
            herdr: file.integrations.herdr && !cli.no_herdr,
            resume_options: resume_options(cli)?,
            workspace,
            debug: cli.debug,
            state_dir,
            session: cli.session.clone(),
            mode: cli.mode.unwrap_or(file.permissions.mode),
            base_url,
            model,
            provider_timeout_seconds,
            context_window,
            ui,
        })
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

/// Retain explicit UI/configuration choices; provider secrets and bootstrap
/// options are deliberately absent. Persisted profiles restore agent/model.
fn resume_options(cli: &Cli) -> Result<Option<Vec<String>>> {
    let mut args = vec![];
    if let Some(required) = cli.require_tool_intent {
        args.push(format!("--require-tool-intent={required}"));
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
        (cli.no_animations, "--no-animations"),
        (cli.native_selection, "--native-selection"),
        (cli.no_logo, "--no-logo"),
        (cli.no_terminal_status, "--no-terminal-status"),
        (cli.no_herdr, "--no-herdr"),
        (cli.no_update_check, "--no-update-check"),
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
