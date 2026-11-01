//! The host environment that agent and user commands receive.
//!
//! roomkit-tools passes only a minimal list by default, so credentials held in
//! the terminal's environment stay out of commands. Development commands also
//! need the variables that locate the user's agents, display, editors, network
//! settings and toolchains: without `SSH_AUTH_SOCK`, `git push` over SSH fails.

use roomkit_tools_process::CommandEnv;

/// Host variables passed beyond the tools' default, when set. None holds a
/// credential by itself: tools reach those through their own agents and
/// configuration. A proxy URL may embed one; commands still need the proxy.
const DEVELOPMENT: &[&str] = &[
    // Authentication agents and desktop services.
    "SSH_AUTH_SOCK",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "DISPLAY",
    "XAUTHORITY",
    "WAYLAND_DISPLAY",
    // Configuration and data locations.
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    // Terminal capabilities and editors.
    "TERM",
    "COLORTERM",
    "EDITOR",
    "VISUAL",
    "GIT_EDITOR",
    // Network.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "REQUESTS_CA_BUNDLE",
    // Container and cluster clients.
    "DOCKER_HOST",
    "KUBECONFIG",
    // Toolchain homes.
    "CARGO_HOME",
    "RUSTUP_HOME",
    "JAVA_HOME",
    "ANDROID_HOME",
    "NVM_DIR",
    "VIRTUAL_ENV",
    "GOPATH",
];

/// The environment of the agent's shell, its processes and `/shell`: the
/// development list and the names the user adds (`--inherit-env`). A
/// `PREFIX*` name is matched against this process's variables now: they do
/// not change after launch, and the tools read values only by exact name.
pub(crate) fn command_env(extra: &[String]) -> CommandEnv {
    let host: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .collect();
    let mut env = CommandEnv::default();
    env.inherit_env
        .extend(DEVELOPMENT.iter().map(|&name| name.to_owned()));
    env.inherit_env.extend(expand(extra, &host));
    env.inherit_env.sort();
    env.inherit_env.dedup();
    env
}

/// Words that mark a variable as a credential. A prefix never passes such a
/// variable, the application's own ROOMKIT_API_KEY included; only its exact
/// name does.
const CREDENTIAL_WORDS: [&str; 6] = ["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL"];

/// Whether `name` reads as a credential.
fn credential(name: &str) -> bool {
    let upper = name.to_uppercase();
    CREDENTIAL_WORDS.iter().any(|word| upper.contains(word))
}

/// `requested` with each `PREFIX*` replaced by the `host` names it starts,
/// credentials left out.
fn expand(requested: &[String], host: &[String]) -> Vec<String> {
    requested
        .iter()
        .flat_map(|name| match name.strip_suffix('*') {
            Some(prefix) => host
                .iter()
                .filter(|candidate| candidate.starts_with(prefix) && !credential(candidate))
                .cloned()
                .collect(),
            None => vec![name.clone()],
        })
        .collect()
}

/// Checks the names a user adds: variable names, or a prefix ending in `*`.
/// A lone `*` would pass every variable, API keys included, and is refused.
pub(crate) fn validate(names: &[String]) -> Result<(), String> {
    for name in names {
        let stem = name.strip_suffix('*').unwrap_or(name);
        if stem.is_empty()
            || stem.contains(['=', '\0', '*', ','])
            || stem.contains(char::is_whitespace)
        {
            return Err(format!(
                "--inherit-env, ROOMKIT_INHERIT_ENV and [shell] inherit_env take variable \
                 names or prefixes such as NIX_*; {name:?} is neither"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn added_names_pass_as_given_and_prefixes_match_the_host_names() {
        let host = [
            "NIX_CFLAGS",
            "NIX_LDFLAGS",
            "NIXOS",
            "PATH",
            "AWS_REGION",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "ROOMKIT_API_KEY",
            "ROOMKIT_UPDATE_TOKEN",
        ]
        .map(String::from);
        let requested = ["LD_LIBRARY_PATH", "NIX_*"].map(String::from);
        assert_eq!(
            expand(&requested, &host),
            ["LD_LIBRARY_PATH", "NIX_CFLAGS", "NIX_LDFLAGS"]
        );
        // A prefix leaves credentials out, the application's own included; an
        // exact name is the user's explicit choice.
        let requested = ["AWS_*", "ROOMKIT_*", "GITHUB_TOKEN"].map(String::from);
        assert_eq!(expand(&requested, &host), ["AWS_REGION", "GITHUB_TOKEN"]);
        assert!(validate(&requested).is_ok());
        for refused in ["", "*", "A=B", "A*B", "TWO WORDS"] {
            assert!(validate(&[refused.into()]).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn the_development_variables_are_valid_names_and_no_credential() {
        let env = command_env(&[]);
        assert!(env.validate().is_ok());
        for name in &env.inherit_env {
            assert!(!credential(name), "{name} looks like a credential");
        }
    }
}
