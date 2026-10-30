//! Private project configuration. Values of environment credentials stay outside it.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::profiles) struct Server {
    pub id: String,
    pub program: String,
    pub args: Vec<String>,
    /// Child variable -> host environment variable. Never a secret value.
    pub environment: BTreeMap<String, String>,
    pub timeout_seconds: u64,
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 24 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn variable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl Server {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err("MCP IDs must contain 1–24 letters, digits or hyphens".into());
        }
        if self.program.trim().is_empty()
            || self.program.len() > 4096
            || self.program.chars().any(char::is_control)
            || serde_json::to_string(&self.args).map_or(true, |json| json.len() > 8192)
            || self.args.len() > 64
            || self.args.iter().any(|a| a.len() > 4096 || a.contains('\0'))
        {
            return Err(
                "Supply a program and at most 64 valid arguments; the JSON array must fit 8192 bytes"
                    .into(),
            );
        }
        if serde_json::to_string(&self.environment).map_or(true, |json| json.len() > 8192)
            || self.environment.len() > 32
            || self
                .environment
                .iter()
                .any(|(key, source)| !variable(key) || !variable(source))
        {
            return Err(
                "Environment must map at most 32 child variable names to host variable names"
                    .into(),
            );
        }
        if !(1..=120).contains(&self.timeout_seconds) {
            return Err("Connection timeout must be between 1 and 120 seconds".into());
        }
        Ok(())
    }

    pub fn command(&self, workspace: &std::path::Path) -> Result<roomkit_mcp::McpCommand, String> {
        self.validate()?;
        let mut command = roomkit_mcp::McpCommand::new(&self.program)
            .args(&self.args)
            .current_dir(workspace)
            .connect_timeout(std::time::Duration::from_secs(self.timeout_seconds));
        for (key, source) in &self.environment {
            let value = std::env::var(source)
                .map_err(|_| "A required MCP environment variable is missing or not Unicode")?;
            if value.contains('\0') {
                return Err("An MCP environment variable contains an invalid value".into());
            }
            command = command.env(key, value);
        }
        Ok(command)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::profiles) struct Servers {
    pub schema: u32,
    pub servers: BTreeMap<String, Server>,
}
impl Default for Servers {
    fn default() -> Self {
        Self {
            schema: 1,
            servers: BTreeMap::new(),
        }
    }
}
impl Servers {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != 1 || self.servers.len() > 32 {
            return Err("Unsupported MCP configuration version or more than 32 servers".into());
        }
        for (id, server) in &self.servers {
            server.validate()?;
            if id != &server.id {
                return Err("MCP server identity does not match its key".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> Server {
        Server {
            id: "fixture".into(),
            program: "fixture-server".into(),
            args: vec![],
            environment: BTreeMap::new(),
            timeout_seconds: 30,
        }
    }

    #[test]
    fn saved_arguments_must_fit_the_editor_after_json_escaping() {
        let mut value = server();
        value.args = vec!["\n".repeat(4096)];
        assert!(
            value.validate().is_err(),
            "JSON escapes also consume form space"
        );
        value.args = vec!["x".repeat(4000), "y".repeat(4000)];
        assert!(value.validate().is_ok());
    }

    #[test]
    fn environment_accepts_references_not_literal_secret_syntax() {
        let mut value = server();
        value
            .environment
            .insert("API_KEY".into(), "MY_SERVICE_KEY".into());
        assert!(value.validate().is_ok());
        value
            .environment
            .insert("API_KEY".into(), "fixture-secret-value".into());
        assert!(value.validate().is_err());
    }

    #[test]
    fn ids_cannot_ambiguously_qualify_another_server() {
        let mut value = server();
        for id in [
            "",
            "bad__alias",
            "bad_",
            "bad.name",
            "too-long-server-identifier",
        ] {
            value.id = id.into();
            assert!(value.validate().is_err());
        }
    }
}
