//! The `--config` file: what each of its tables holds, and its defaults.
use serde::{Deserialize, Serialize};

use super::{Color, Palette, Result};
use crate::permissions::Mode;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct FileConfig {
    pub(super) agent: AgentOptions,
    pub(super) memory: crate::project_memory::Config,
    pub(super) provider: Provider,
    pub(super) ui: Appearance,
    pub(super) permissions: Permissions,
    pub(super) integrations: Integrations,
    pub(super) updates: UpdateOptions,
    pub(super) shell: ShellOptions,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct ShellOptions {
    pub(super) inherit_env: Vec<String>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct UpdateOptions {
    pub(super) check: bool,
}
impl Default for UpdateOptions {
    fn default() -> Self {
        Self { check: true }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct AgentOptions {
    pub(super) require_tool_intent: bool,
    pub(super) max_tool_rounds: Option<u32>,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            require_tool_intent: true,
            max_tool_rounds: None,
        }
    }
}

/// Tool-call rounds available to an interactive or executor turn by default.
pub(crate) const DEFAULT_TOOL_ROUNDS: u32 = 200;
/// Unattended, nobody can say to continue: a turn runs until it answers,
/// as other harnesses' turns do (HITL 1603bb98).
pub(crate) const UNATTENDED_TOOL_ROUNDS: u32 = u32::MAX;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Integrations {
    pub(super) herdr: bool,
}

impl Default for Integrations {
    fn default() -> Self {
        Self { herdr: true }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Permissions {
    pub(super) mode: Mode,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Provider {
    pub(super) kind: Option<crate::providers::ProviderKind>,
    pub(super) base_url: Option<String>,
    pub(super) model: Option<String>,
    pub(super) timeout_seconds: Option<u64>,
    pub(super) context_window: Option<u64>,
    pub(super) max_output_tokens: Option<u32>,
    pub(super) thinking_budget: Option<u32>,
    pub(super) reasoning_effort: Option<String>,
    pub(super) pricing: Option<Pricing>,
}

/// The configured model's list price per million tokens, for a server whose
/// model the kit's catalogue does not price.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pricing {
    pub(super) input_per_million: f64,
    pub(super) output_per_million: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cache_read_per_million: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cache_write_per_million: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) currency: Option<String>,
    /// The day the rates were read, as `YYYY-MM-DD`.
    pub(super) verified: String,
}

impl Pricing {
    /// The kit's price, its rates checked as the kit checks its catalogue's.
    pub(super) fn read(self) -> Result<roomkit_ai::ModelPricing> {
        serde_json::to_value(self)
            .ok()
            .and_then(|fields| serde_json::from_value(fields).ok())
            .ok_or_else(|| {
                "[provider.pricing] needs rates of 0 or more per million tokens and \
                 verified = \"YYYY-MM-DD\""
                    .into()
            })
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Appearance {
    pub(super) welcome: bool,
    pub(super) welcome_width: u16,
    pub(super) welcome_logo: Option<Vec<String>>,
    pub(super) welcome_wordmark: Option<Vec<String>>,
    pub(super) bottom_margin: u16,
    pub(super) sidebar: bool,
    pub(super) sidebar_width: u16,
    pub(super) terminal_status: bool,
    pub(super) theme: Palette,
    pub(super) color: Color,
    pub(super) animations: bool,
    pub(super) native_selection: bool,
    pub(super) name: String,
    pub(super) logo: Option<Vec<String>>,
    pub(super) markdown: bool,
    pub(super) message_gap: u16,
    pub(super) highlight_own_messages: bool,
    pub(super) tool_preview_lines: usize,
    pub(super) tool_activity: bool,
    pub(super) input_min_lines: u16,
    pub(super) input_padding: u16,
    pub(super) input_placeholder: String,
    pub(super) input_prompt: String,
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
