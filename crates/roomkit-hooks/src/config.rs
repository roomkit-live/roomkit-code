use std::collections::BTreeMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Events implemented by the first command-hook contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Event {
    /// Session execution opened or resumed, not merely a view attachment.
    SessionStart,
    /// Session execution ended; durable conversation remains available.
    SessionEnd,
    /// User input checked before entering conversation history.
    UserPromptSubmit,
    /// A logical agent turn began, before its first model call.
    TurnStart,
    /// A logical turn definitively completed, failed or was interrupted.
    TurnEnd,
    /// Before an agent tool runs; may deny or require human review.
    PreToolUse,
    /// A tool succeeded; observation cannot reverse its effects.
    PostToolUse,
    /// A tool failed, was refused or cancelled; exact outcome is in the payload.
    PostToolUseFailure,
}

impl Event {
    /// Stable external spelling.
    pub fn name(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::SessionEnd => "SessionEnd",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::TurnStart => "TurnStart",
            Self::TurnEnd => "TurnEnd",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
        }
    }

    /// Whether this event can prevent admission or an action.
    pub fn blocking(self) -> bool {
        matches!(self, Self::UserPromptSubmit | Self::PreToolUse)
    }
}

/// Explicit RoomKit extensions; omitted fields preserve conservative defaults.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Options {
    /// Permit this command in Plan mode. Commands are not a read-only sandbox.
    pub run_in_plan: bool,
}

/// A shell command supplied by trusted configuration, never model interpolation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandHook {
    /// Only `command` is implemented. Other handler kinds are rejected.
    #[serde(rename = "type")]
    pub kind: String,
    /// Command interpreted by /bin/sh with JSON input on stdin.
    pub command: String,
    /// Maximum seconds, including stdin/output handling; default ten, maximum thirty.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    /// Optional short activity label.
    #[serde(default, rename = "statusMessage")]
    pub status_message: Option<String>,
    /// Explicit non-portable options.
    #[serde(default)]
    pub roomkit: Options,
}

fn default_timeout() -> u64 {
    10
}

/// Matching commands, in declaration order.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookGroup {
    /// Regex over canonical tool names, session source or end reason. Empty or
    /// `*` matches everything. Other events accept only empty or `*`.
    #[serde(default)]
    pub matcher: String,
    /// Handlers run sequentially, so blocking decisions are deterministic.
    pub hooks: Vec<CommandHook>,
}

impl HookGroup {
    /// Test a validated matcher against a host-provided event value.
    pub fn matches(&self, value: &str) -> bool {
        self.matcher.is_empty()
            || self.matcher == "*"
            || Regex::new(&self.matcher).is_ok_and(|regex| regex.is_match(value))
    }
}

/// Familiar event → matcher → handlers configuration, rejecting unknown fields.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    /// Supported events and their handlers. Unknown events are errors, not no-ops.
    #[serde(default)]
    pub hooks: BTreeMap<Event, Vec<HookGroup>>,
}

impl Configuration {
    /// Parse and bound a configuration. Error messages omit source contents.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 131_072 {
            return Err("Hook configuration exceeds 128 KiB".into());
        }
        let config: Self = serde_json::from_slice(bytes).map_err(|error| {
            format!("Invalid hook configuration at line {}, column {}; check supported events and fields", error.line(), error.column())
        })?;
        let mut count = 0;
        for (event, groups) in &config.hooks {
            let mut budget = 0;
            for group in groups {
                if group.matcher.len() > 512 {
                    return Err("Hook matcher exceeds 512 bytes".into());
                }
                if !group.matcher.is_empty() && group.matcher != "*" {
                    if matches!(
                        event,
                        Event::UserPromptSubmit | Event::TurnStart | Event::TurnEnd
                    ) {
                        return Err(format!("{} does not support matchers", event.name()));
                    }
                    Regex::new(&group.matcher).map_err(|_| "Invalid hook matcher regex")?;
                }
                if group.hooks.is_empty() {
                    return Err("Hook group has no handlers".into());
                }
                for hook in &group.hooks {
                    count += 1;
                    if hook.kind != "command" {
                        return Err("Only command hooks are supported".into());
                    }
                    if hook.command.trim().is_empty()
                        || hook.command.len() > 8192
                        || hook.command.contains('\0')
                    {
                        return Err("Hook command must contain 1–8192 bytes without NUL".into());
                    }
                    if !(1..=30).contains(&hook.timeout) {
                        return Err("Hook timeout must be 1–30 seconds".into());
                    }
                    budget += hook.timeout;
                    if hook
                        .status_message
                        .as_ref()
                        .is_some_and(|s| s.len() > 128 || s.chars().any(char::is_control))
                    {
                        return Err(
                            "Hook statusMessage must be a single line of at most 128 bytes".into(),
                        );
                    }
                }
            }
            if budget > 60 {
                return Err(format!(
                    "{} handlers exceed the 60-second budget per source",
                    event.name()
                ));
            }
        }
        if count > 64 {
            return Err("At most 64 hook handlers per source are supported".into());
        }
        Ok(config)
    }
}
