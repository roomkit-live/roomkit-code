//! The command line: every option of a launch.
use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use serde::Deserialize;

use crate::permissions::Mode;

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
    /// Offer the remote-linux delegation target in this Redis group.
    #[arg(long, conflicts_with_all = ["serve_worker", "serve_pool", "executor_pool", "prompt"], value_parser = crate::remote::group)]
    pub remote_worker: Option<String>,
    /// Serve the remote-linux agent for this Redis group from this workspace.
    #[arg(long, conflicts_with_all = ["prompt", "session", "serve_pool", "executor_pool"], value_parser = crate::remote::group)]
    pub serve_worker: Option<String>,
    /// Schedule file tasks across workers registered in this Redis group.
    #[arg(long, conflicts_with_all = ["serve_pool", "prompt"], value_parser = crate::remote::group)]
    pub executor_pool: Option<String>,
    /// Serve scheduled tasks in this Redis group.
    #[arg(long, conflicts_with_all = ["prompt", "session"], value_parser = crate::remote::group)]
    pub serve_pool: Option<String>,
    /// Serve this workspace with the worker settings saved in /executors.
    #[arg(long, conflicts_with_all = ["serve_pool", "serve_worker", "executor_pool", "remote_worker", "prompt", "session", "local"])]
    pub executor: bool,
    /// Open this project without its saved executor pool (also works offline).
    #[arg(long, conflicts_with_all = ["serve_pool", "serve_worker", "executor_pool", "remote_worker"])]
    pub local: bool,
    #[command(flatten)]
    pub worker: crate::remote::pool::WorkerArgs,
    /// Project directory used by the coding tools.
    #[arg(default_value = ".")]
    pub workspace: PathBuf,
    /// Explicit TOML configuration file (never loaded from the project implicitly).
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Initial provider protocol; existing saved connections take precedence.
    #[arg(long, env = "ROOMKIT_PROVIDER", value_enum)]
    pub provider: Option<crate::providers::ProviderKind>,
    /// Initial API root for a project without agent profiles.
    #[arg(long, env = "ROOMKIT_BASE_URL", hide_env_values = true)]
    pub base_url: Option<String>,
    /// Initial model for a project without agent profiles.
    #[arg(long, env = "ROOMKIT_MODEL", hide_env_values = true)]
    pub model: Option<String>,
    /// Initial model-request timeout: the longest wait between two reads of its answer.
    #[arg(long)]
    pub provider_timeout_seconds: Option<u64>,
    /// Initial effective model context capacity in tokens.
    #[arg(long)]
    pub context_window: Option<u64>,
    /// Fixed reply token limit, including reasoning; omitted selects Auto.
    #[arg(long)]
    pub max_output_tokens: Option<u32>,
    /// llama.cpp reasoning token limit within the reply budget (0 disables reasoning).
    #[arg(long)]
    pub thinking_budget: Option<u32>,
    /// Initial reasoning effort; the provider's default when omitted.
    #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(crate::profiles::model::EFFORTS))]
    pub reasoning_effort: Option<String>,
    /// Open or create a named conversation; without this, start a new session.
    #[arg(long)]
    pub session: Option<String>,
    /// Advertise this named interactive session to a trusted Redis group (experimental).
    #[arg(long, requires = "session", conflicts_with_all = ["prompt", "local"], value_parser = crate::remote::group)]
    pub session_group: Option<String>,
    /// Send this message without the terminal interface, print the agent's
    /// answer and exit; `-` reads the message from standard input. Nobody
    /// answers approvals: they are refused.
    #[arg(short, long, conflicts_with = "check_config")]
    pub prompt: Option<String>,
    /// With --prompt, write every event of the conversation to standard
    /// output as one JSON object per line instead of the answer.
    #[arg(long, requires = "prompt")]
    pub json: bool,
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
    /// Rounds of tool calls one turn may run before it stops unfinished:
    /// 200 by default, no limit with --prompt.
    #[arg(long)]
    pub max_tool_rounds: Option<u32>,
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
    /// More host variables for shell commands, comma-separated; `NIX_*`
    /// passes every variable with that prefix, credentials left out.
    /// Replaces ROOMKIT_INHERIT_ENV, which replaces `[shell] inherit_env`.
    #[arg(long, value_delimiter = ',')]
    pub inherit_env: Vec<String>,
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
