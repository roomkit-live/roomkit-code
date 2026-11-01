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
    // Toolchain homes.
    "CARGO_HOME",
    "RUSTUP_HOME",
    "JAVA_HOME",
    "ANDROID_HOME",
    "NVM_DIR",
    "VIRTUAL_ENV",
    "GOPATH",
];

/// The environment of the agent's shell, its processes and `/shell`.
pub(crate) fn command_env() -> CommandEnv {
    let mut env = CommandEnv::default();
    env.inherit_env
        .extend(DEVELOPMENT.iter().map(|&name| name.to_owned()));
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_development_variables_are_valid_names_and_no_credential() {
        let env = command_env();
        assert!(env.validate().is_ok());
        let credential = ["KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL"];
        for name in &env.inherit_env {
            let upper = name.to_uppercase();
            assert!(
                !credential.iter().any(|word| upper.contains(word)),
                "{name} looks like a credential"
            );
        }
    }
}
