//! Explicit application configuration; repository files are never auto-loaded.

use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use roomkit_tui::{Branding, ColorMode, SelectionMode, Theme, UiOptions};
use serde::Deserialize;

use crate::Result;

#[derive(Debug, Parser)]
#[command(version, about = "Explore a project with a RoomKit coding assistant")]
pub(crate) struct Cli {
    /// Project directory granted to the read-only tools.
    #[arg(default_value = ".")]
    pub workspace: PathBuf,
    /// Explicit TOML configuration file (never loaded from the project implicitly).
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// OpenAI-compatible API root; overrides environment and TOML.
    #[arg(long, env = "ROOMKIT_BASE_URL", hide_env_values = true)]
    pub base_url: Option<String>,
    /// Model identifier served by the configured endpoint.
    #[arg(long, env = "ROOMKIT_MODEL", hide_env_values = true)]
    pub model: Option<String>,
    /// Conversation name within this project; reuse it to resume.
    #[arg(long, default_value = "main")]
    pub session: String,
    /// Storage directory; defaults to the user's application data directory.
    #[arg(long, env = "ROOMKIT_CODE_STATE_DIR", hide_env_values = true)]
    pub state_dir: Option<PathBuf>,
    /// Check configuration without opening a database, terminal or connection.
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
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Provider {
    base_url: Option<String>,
    model: Option<String>,
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
        }
    }
}

pub(crate) struct Settings {
    pub workspace: PathBuf,
    pub state_dir: PathBuf,
    pub session: String,
    pub base_url: String,
    pub model: String,
    pub ui: UiOptions,
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
            .ok_or("Set --base-url, ROOMKIT_BASE_URL or provider.base_url in --config")?;
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
        let model = cli
            .model
            .clone()
            .or(file.provider.model)
            .filter(|value| !value.trim().is_empty())
            .ok_or("Set --model, ROOMKIT_MODEL or provider.model in --config")?;
        if cli.session.is_empty()
            || cli.session.len() > 64
            || !cli
                .session
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(
                "Session names use 1–64 ASCII letters, digits, hyphens or underscores".into(),
            );
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
            subtitle: "Read-only project exploration".into(),
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
            base_url,
            model,
            ui,
        })
    }
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
