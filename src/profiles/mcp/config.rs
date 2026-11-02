//! Private project configuration. Credentials are resolved only at connection time.
use roomkit_mcp::{FromCommandOptions, FromUrlOptions, McpToolProvider};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::profiles) struct Server {
    pub id: String,
    pub transport: Transport,
    pub timeout_seconds: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::profiles) enum Transport {
    Stdio {
        program: String,
        args: Vec<String>,
        environment: BTreeMap<String, String>,
    },
    Http {
        url: String,
        bearer_env: Option<String>,
        headers: BTreeMap<String, String>,
    },
}
impl Transport {
    pub fn is_http(&self) -> bool {
        matches!(self, Self::Http { .. })
    }
    pub fn empty(http: bool) -> Self {
        if http {
            Self::Http {
                url: String::new(),
                bearer_env: None,
                headers: BTreeMap::new(),
            }
        } else {
            Self::Stdio {
                program: String::new(),
                args: vec![],
                environment: BTreeMap::new(),
            }
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Stdio {
                program,
                args,
                environment,
            } => {
                if program.trim().is_empty()
                    || program.len() > 4096
                    || program.chars().any(char::is_control)
                    || !fits_editor(args)
                    || args.len() > 64
                    || args.iter().any(|a| a.len() > 4096 || a.contains('\0'))
                {
                    return Err("Supply a program and at most 64 valid arguments; the JSON array must fit 8192 bytes".into());
                }
                if !fits_editor(environment)
                    || environment.len() > 32
                    || environment
                        .iter()
                        .any(|(k, v)| !variable(k) || !variable(v))
                {
                    return Err("Environment must map at most 32 child variable names to host variable names".into());
                }
            }
            Self::Http {
                url,
                bearer_env,
                headers,
            } => {
                if url.len() > 4096 || url.chars().any(char::is_control) {
                    return Err("Invalid MCP endpoint URL".into());
                }
                if endpoint(url, BTreeMap::new()).is_none() {
                    return Err(
                        "Use an HTTP(S) MCP endpoint without URL credentials or a fragment".into(),
                    );
                }
                if bearer_env.as_ref().is_some_and(|v| !variable(v)) {
                    return Err(
                        "Bearer token must reference a host environment variable name".into(),
                    );
                }
                if headers.len() > 32
                    || !fits_editor(headers)
                    || headers.values().any(|v| !variable(v))
                {
                    return Err(
                        "Headers must map at most 32 HTTP header names to host variable names"
                            .into(),
                    );
                }
                let mut names = std::collections::BTreeSet::new();
                let mut sent = BTreeMap::new();
                for name in headers.keys() {
                    if !names.insert(name.to_ascii_lowercase())
                        || (bearer_env.is_some() && name.eq_ignore_ascii_case("authorization"))
                    {
                        return Err(
                            "Duplicate HTTP header; configure Authorization only once".into()
                        );
                    }
                    sent.insert(name.clone(), "validation".to_owned());
                    if endpoint(url, sent.clone()).is_none() {
                        return Err("Invalid or transport-owned HTTP header".into());
                    }
                }
                if bearer_env.is_some() {
                    sent.insert("authorization".into(), "Bearer validation".into());
                    if endpoint(url, sent).is_none() {
                        return Err("Too many HTTP headers".into());
                    }
                }
            }
        }
        Ok(())
    }
}
/// The endpoint `url`, sending `headers`, as the MCP client accepts it;
/// none for a URL or a header it refuses.
fn endpoint(url: &str, headers: BTreeMap<String, String>) -> Option<McpToolProvider> {
    let options = FromUrlOptions {
        headers,
        ..FromUrlOptions::default()
    };
    McpToolProvider::from_url(url, options).ok()
}
fn fits_editor(value: &impl Serialize) -> bool {
    serde_json::to_string(value).is_ok_and(|s| s.len() <= 8192)
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
fn credential(source: &str) -> Result<String, String> {
    let value = std::env::var(source)
        .map_err(|_| "A required MCP environment variable is missing or not Unicode")?;
    if value.contains('\0') {
        return Err("An MCP environment variable contains an invalid value".into());
    }
    Ok(value)
}
impl Server {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err("MCP IDs must contain 1–24 letters, digits or hyphens".into());
        }
        if !(1..=120).contains(&self.timeout_seconds) {
            return Err("Connection timeout must be between 1 and 120 seconds".into());
        }
        self.transport.validate()
    }
    /// The server as configured, its tools served under `mcp__<id>__<tool>`,
    /// not connected yet; its credentials are read from the host's
    /// environment now.
    pub(super) fn connection(
        &self,
        workspace: &std::path::Path,
    ) -> Result<McpToolProvider, String> {
        self.validate()?;
        let connect_timeout = Duration::from_secs(self.timeout_seconds);
        let label = Some(self.id.clone());
        match &self.transport {
            Transport::Stdio {
                program,
                args,
                environment,
            } => {
                let mut env = BTreeMap::new();
                for (key, source) in environment {
                    env.insert(key.into(), credential(source)?.into());
                }
                let options = FromCommandOptions {
                    args: args.iter().map(Into::into).collect(),
                    env,
                    cwd: Some(workspace.to_path_buf()),
                    label,
                    connect_timeout,
                    ..FromCommandOptions::default()
                };
                McpToolProvider::from_command(program, options)
                    .map_err(|_| "Invalid MCP configuration".into())
            }
            Transport::Http {
                url,
                bearer_env,
                headers,
            } => {
                if endpoint(url, BTreeMap::new()).is_none() {
                    return Err("Invalid MCP endpoint".into());
                }
                let mut sent = BTreeMap::new();
                for (name, source) in headers {
                    sent.insert(name.clone(), credential(source)?);
                    if endpoint(url, sent.clone()).is_none() {
                        return Err(
                            "An MCP header environment variable contains an invalid value".into(),
                        );
                    }
                }
                if let Some(source) = bearer_env {
                    let token = credential(source)?;
                    if token.is_empty() {
                        return Err("The MCP bearer environment variable is empty".into());
                    }
                    sent.insert("authorization".into(), format!("Bearer {token}"));
                }
                let options = FromUrlOptions {
                    headers: sent,
                    label,
                    connect_timeout,
                    ..FromUrlOptions::default()
                };
                McpToolProvider::from_url(url, options).map_err(|_| {
                    "The MCP bearer environment variable contains an invalid value".into()
                })
            }
        }
    }
}
/// `mcp.json`, schema 2: another schema is refused when the file is read,
/// before it replaces the loaded configuration.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::profiles) struct Servers {
    #[serde(deserialize_with = "current_schema")]
    pub schema: u32,
    pub servers: BTreeMap<String, Server>,
}
impl Default for Servers {
    fn default() -> Self {
        Self {
            schema: 2,
            servers: BTreeMap::new(),
        }
    }
}
fn current_schema<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let schema = u32::deserialize(deserializer)?;
    if schema != 2 {
        return Err(serde::de::Error::custom(
            "Unsupported MCP configuration version",
        ));
    }
    Ok(schema)
}

impl Servers {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != 2 || self.servers.len() > 32 {
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
    #![expect(clippy::unwrap_used, reason = "fixture failures must fail the test")]
    use super::*;

    fn server() -> Server {
        Server {
            id: "fixture".into(),
            transport: Transport::Stdio {
                program: "fixture-server".into(),
                args: vec![],
                environment: BTreeMap::new(),
            },
            timeout_seconds: 30,
        }
    }

    #[test]
    fn saved_arguments_must_fit_the_editor_after_json_escaping() {
        let mut value = server();
        let Transport::Stdio { args, .. } = &mut value.transport else {
            unreachable!()
        };
        *args = vec!["\n".repeat(4096)];
        assert!(
            value.validate().is_err(),
            "JSON escapes also consume form space"
        );
        let Transport::Stdio { args, .. } = &mut value.transport else {
            unreachable!()
        };
        *args = vec!["x".repeat(4000), "y".repeat(4000)];
        assert!(value.validate().is_ok());
    }

    #[test]
    fn environment_accepts_references_not_literal_secret_syntax() {
        let mut value = server();
        let Transport::Stdio { environment, .. } = &mut value.transport else {
            unreachable!()
        };
        environment.insert("API_KEY".into(), "MY_SERVICE_KEY".into());
        assert!(value.validate().is_ok());
        let Transport::Stdio { environment, .. } = &mut value.transport else {
            unreachable!()
        };
        environment.insert("API_KEY".into(), "fixture-secret-value".into());
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
    #[test]
    fn a_configuration_of_another_schema_is_refused_when_read() {
        for schema in [1, 3] {
            let file = serde_json::json!({"schema": schema, "servers": {}});
            assert!(serde_json::from_value::<Servers>(file).is_err());
        }
        let current = serde_json::json!({"schema": 2, "servers": {}});
        assert!(serde_json::from_value::<Servers>(current).is_ok());
    }
    #[test]
    fn rejects_ambiguous_headers_and_literal_credentials() {
        let mut transport = Transport::Http {
            url: "http://localhost/mcp".into(),
            bearer_env: Some("MY_MCP_TOKEN".into()),
            headers: BTreeMap::new(),
        };
        assert!(transport.validate().is_ok());
        if let Transport::Http { headers, .. } = &mut transport {
            headers.insert("Authorization".into(), "MY_TOKEN".into());
        }
        assert!(transport.validate().is_err());
        if let Transport::Http {
            headers,
            bearer_env,
            ..
        } = &mut transport
        {
            headers.clear();
            *bearer_env = Some("Bearer secret".into());
        }
        assert!(transport.validate().is_err());
        if let Transport::Http {
            headers,
            bearer_env,
            ..
        } = &mut transport
        {
            *bearer_env = None;
            headers.insert("Mcp-Session-Id".into(), "MY_SESSION".into());
        }
        assert!(transport.validate().is_err());
    }
    #[test]
    fn missing_http_credential_is_reported_without_its_name_or_value() {
        let server = Server {
            id: "fixture".into(),
            timeout_seconds: 30,
            transport: Transport::Http {
                url: "http://localhost/mcp".into(),
                bearer_env: Some("ROOMKIT_MCP_MISSING_CREDENTIAL_FIXTURE".into()),
                headers: BTreeMap::new(),
            },
        };
        let error = server.connection(std::path::Path::new(".")).err().unwrap();
        assert!(error.contains("missing"));
        assert!(!error.contains("ROOMKIT_MCP_MISSING"));
    }
}
