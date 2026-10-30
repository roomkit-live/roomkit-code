//! Explicit application configuration; repository files are never auto-loaded.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use roomkit_tui::{Branding, ColorMode, SelectionMode, Theme, UiOptions};
use serde::Deserialize;

use crate::{Result, permissions::Mode};

#[derive(Debug, Parser)]
#[command(version, about = "Explore a project with a RoomKit coding assistant")]
pub(crate) struct Cli {
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
    /// Permission mode; ask approves each mutation/command, yolo runs directly.
    #[arg(long, value_enum)]
    pub mode: Option<Mode>,
    /// Storage directory; defaults to the user's application data directory.
    #[arg(long, env = "ROOMKIT_CODE_STATE_DIR", hide_env_values = true)]
    pub state_dir: Option<PathBuf>,
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
    provider: Provider,
    ui: Appearance,
    permissions: Permissions,
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
    theme: Palette,
    color: Color,
    animations: bool,
    native_selection: bool,
    name: String,
    logo: Option<Vec<String>>,
    markdown: bool,
    message_gap: u16,
    tool_preview_lines: usize,
    input_min_lines: u16,
    input_padding: u16,
    input_placeholder: String,
    input_prompt: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: Palette::default(),
            color: Color::default(),
            animations: true,
            native_selection: false,
            name: "RoomKit Code".into(),
            logo: None,
            markdown: true,
            message_gap: 1,
            tool_preview_lines: 4,
            input_min_lines: 1,
            input_padding: 1,
            input_placeholder: "Ask about this project…".into(),
            input_prompt: "›".into(),
        }
    }
}

pub(crate) struct Settings {
    pub workspace: PathBuf,
    pub state_dir: PathBuf,
    pub session: Option<String>,
    pub base_url: String,
    pub model: String,
    pub provider_timeout_seconds: u64,
    pub context_window: Option<u64>,
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
            .map_err(|_| "Workspace does not exist")?;
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
        let mut branding = Branding {
            name: appearance.name,
            subtitle: "Explore, edit and test your project".into(),
            ..Branding::default()
        };
        if let Some(logo) = appearance.logo {
            branding.logo = logo;
        }
        if cli.no_logo {
            branding.logo.clear();
        }
        let ui = UiOptions {
            branding,
            theme: match cli.theme.unwrap_or(appearance.theme) {
                Palette::Dark => Theme::dark(),
                Palette::Light => Theme::light(),
                Palette::Terminal => Theme::terminal(),
            },
            color: match cli.color.unwrap_or(appearance.color) {
                Color::Auto => ColorMode::Auto,
                Color::Always => ColorMode::Always,
                Color::Never => ColorMode::Never,
            },
            animations: appearance.animations && !cli.no_animations,
            markdown: appearance.markdown,
            message_gap: appearance.message_gap,
            tool_preview_lines: appearance.tool_preview_lines,
            input_min_lines: appearance.input_min_lines,
            input_padding: appearance.input_padding,
            input_placeholder: appearance.input_placeholder,
            input_prompt: appearance.input_prompt,
            selection: if cli.native_selection || appearance.native_selection {
                SelectionMode::Native
            } else {
                SelectionMode::CopyOnSelect
            },
            ..UiOptions::default()
        };
        Ok(Self {
            workspace,
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

fn default_state_dir() -> Option<PathBuf> {
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

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
