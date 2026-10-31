#![expect(clippy::unwrap_used, reason = "fixture failures must fail the test")]

use super::*;

fn cli(root: &std::path::Path) -> Cli {
    Cli::try_parse_from([
        "roomkit-code",
        root.to_str().unwrap(),
        "--base-url",
        "http://localhost:8000/v1",
        "--model",
        "fixture",
    ])
    .unwrap()
}

#[test]
fn resume_options_exclude_provider_bootstrap_values() {
    let root = tempfile::tempdir().unwrap();
    let mut cli = cli(root.path());
    cli.base_url = Some("https://example.invalid/private-provider".into());
    cli.model = Some("private-model".into());
    cli.no_logo = true;
    assert_eq!(resume_options(&cli).unwrap().unwrap(), ["--no-logo"]);
}

#[test]
fn required_intents_are_opt_in_and_cli_overrides_survive_resume() {
    let root = tempfile::tempdir().unwrap();
    let mut cli = cli(root.path());
    assert!(!Settings::load(&cli).unwrap().require_tool_intent);
    let config = root.path().join("options.toml");
    std::fs::write(&config, "[agent]\nrequire_tool_intent = true\n").unwrap();
    cli.config = Some(config.clone());
    assert!(Settings::load(&cli).unwrap().require_tool_intent);
    for required in [false, true] {
        let flag = format!("--require-tool-intent={required}");
        let parsed = Cli::try_parse_from([
            "roomkit-code",
            root.path().to_str().unwrap(),
            "--config",
            config.to_str().unwrap(),
            &flag,
        ])
        .unwrap();
        assert_eq!(
            Settings::load(&parsed).unwrap().require_tool_intent,
            required
        );
        let resumed = Cli::try_parse_from(
            [
                "roomkit-code".to_owned(),
                root.path().to_str().unwrap().to_owned(),
            ]
            .into_iter()
            .chain(resume_options(&parsed).unwrap().unwrap()),
        )
        .unwrap();
        assert_eq!(
            Settings::load(&resumed).unwrap().require_tool_intent,
            required
        );
    }
    let parsed = Cli::try_parse_from([
        "roomkit-code",
        "--require-tool-intent",
        root.path().to_str().unwrap(),
    ])
    .unwrap();
    assert!(Settings::load(&parsed).unwrap().require_tool_intent);
}

#[test]
fn terminal_and_herdr_integrations_are_independently_optional() {
    let root = tempfile::tempdir().unwrap();
    let mut cli = cli(root.path());
    let settings = Settings::load(&cli).unwrap();
    assert!(settings.herdr && settings.ui.terminal_status.is_some());
    cli.no_herdr = true;
    assert!(!Settings::load(&cli).unwrap().herdr);
    assert!(Settings::load(&cli).unwrap().ui.terminal_status.is_some());
    cli.no_herdr = false;
    cli.no_terminal_status = true;
    assert!(Settings::load(&cli).unwrap().herdr);
    assert!(Settings::load(&cli).unwrap().ui.terminal_status.is_none());
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        "[ui]\nterminal_status = false\n[integrations]\nherdr = false\n",
    )
    .unwrap();
    cli.config = Some(config);
    cli.no_terminal_status = false;
    let settings = Settings::load(&cli).unwrap();
    assert!(!settings.herdr && settings.ui.terminal_status.is_none());
}

#[test]
fn explicit_configuration_and_cli_overrides_control_appearance_without_creating_state() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    std::fs::write(&path, "[provider]\nbase_url = 'http://localhost:9000/v1'\nmodel = 'from-file'\n[ui]\ntheme = 'light'\nname = 'Example Code'\nlogo = ['AB']\nanimations = false\n").unwrap();
    let mut cli = cli(root.path());
    cli.config = Some(path);
    cli.state_dir = Some(root.path().join("uncreated"));
    let loaded = Settings::load(&cli).unwrap();
    assert_eq!(loaded.model, "fixture");
    assert_eq!(loaded.ui.branding.name, "Example Code");
    assert_eq!(loaded.ui.branding.logo, ["AB"]);
    assert_eq!(loaded.ui.theme, Theme::light());
    assert!(!loaded.ui.animations);
    cli.model = None;
    cli.base_url = None;
    cli.theme = Some(Palette::Terminal);
    cli.no_logo = true;
    cli.native_selection = true;
    let loaded = Settings::load(&cli).unwrap();
    assert_eq!(loaded.model, "from-file");
    assert_eq!(loaded.base_url, "http://localhost:9000/v1");
    assert_eq!(loaded.ui.theme, Theme::terminal());
    assert!(loaded.ui.branding.logo.is_empty());
    assert_eq!(loaded.ui.selection, SelectionMode::Native);
    assert!(!loaded.state_dir.exists());
}

#[test]
fn invalid_or_credential_bearing_settings_are_refused_without_echoing_contents() {
    let root = tempfile::tempdir().unwrap();
    let mut cli = cli(root.path());
    for url in [
        "file:///tmp/project",
        "https://user:private-value@localhost/v1",
        "http://localhost/v1?key=private-value",
        "http://localhost/#private-value",
    ] {
        cli.base_url = Some(url.into());
        assert!(
            Settings::load(&cli)
                .err()
                .unwrap()
                .to_string()
                .contains("HTTP(S)")
        );
    }
    cli.base_url = Some("http://localhost/v1".into());
    cli.session = Some("../other".into());
    assert!(Settings::load(&cli).is_err());
    cli.session = Some("main".into());
    let config = root.path().join("config.toml");
    std::fs::write(&config, "[provider]\napi_key = 'private-value'\n").unwrap();
    cli.config = Some(config);
    let error = Settings::load(&cli).err().unwrap().to_string();
    assert!(error.contains("Invalid configuration"));
    assert!(!error.contains("private-value"));
}

#[test]
fn project_configuration_is_not_implicitly_trusted() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("roomkit-code.toml"),
        "[provider]\nmodel = 'untrusted'\n",
    )
    .unwrap();
    let mut cli = cli(root.path());
    cli.model = None;
    assert!(Settings::load(&cli).is_err());
    cli.model = Some("fixture".into());
    cli.workspace = root.path().join("roomkit-code.toml");
    assert!(Settings::load(&cli).is_err());
}

#[test]
fn permission_mode_defaults_to_ask_and_only_explicit_settings_enable_yolo() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("roomkit-code.toml");
    std::fs::write(&path, "[permissions]\nmode = 'yolo'\n").unwrap();
    let mut cli = cli(root.path());
    assert_eq!(Settings::load(&cli).unwrap().mode, Mode::Ask);
    cli.config = Some(path.clone());
    assert_eq!(Settings::load(&cli).unwrap().mode, Mode::Yolo);
    cli.mode = Some(Mode::Ask);
    assert_eq!(Settings::load(&cli).unwrap().mode, Mode::Ask);
    std::fs::write(&path, "[permissions]\nmode = 'unknown'\n").unwrap();
    assert!(Settings::load(&cli).is_err());
}

#[cfg(unix)]
#[test]
fn symlink_aliases_resolve_to_one_project() {
    let root = tempfile::tempdir().unwrap();
    let link = root.path().join("alias");
    std::os::unix::fs::symlink(root.path(), &link).unwrap();
    assert_eq!(
        Settings::load(&cli(root.path())).unwrap().workspace,
        Settings::load(&cli(&link)).unwrap().workspace
    );
}

#[test]
fn provider_limits_are_explicit_and_cli_values_override_the_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.toml");
    std::fs::write(
        &path,
        "[provider]\ntimeout_seconds = 240\ncontext_window = 32768\n",
    )
    .unwrap();
    let mut cli = cli(root.path());
    assert_eq!(Settings::load(&cli).unwrap().provider_timeout_seconds, 600);
    assert_eq!(Settings::load(&cli).unwrap().context_window, None);
    cli.config = Some(path);
    let file = Settings::load(&cli).unwrap();
    assert_eq!(
        (file.provider_timeout_seconds, file.context_window),
        (240, Some(32768))
    );
    cli.provider_timeout_seconds = Some(900);
    cli.context_window = Some(65536);
    let flags = Settings::load(&cli).unwrap();
    assert_eq!(
        (flags.provider_timeout_seconds, flags.context_window),
        (900, Some(65536))
    );
    cli.context_window = Some(0);
    assert!(Settings::load(&cli).is_err());
    cli.context_window = None;
    cli.provider_timeout_seconds = Some(0);
    assert!(Settings::load(&cli).is_err());
}

#[test]
fn sidebar_appearance_is_configurable_without_changing_agent_settings() {
    let root = tempfile::tempdir().unwrap();
    let mut cli = cli(root.path());
    assert!(Settings::load(&cli).unwrap().ui.sidebar_visible);
    let config = root.path().join("appearance.toml");
    std::fs::write(&config, "[ui]\nsidebar = false\nsidebar_width = 42\n").unwrap();
    cli.config = Some(config);
    let settings = Settings::load(&cli).unwrap();
    assert!(!settings.ui.sidebar_visible);
    assert_eq!(settings.ui.sidebar_width, 42);
}

#[test]
fn welcome_artwork_and_bottom_spacing_follow_host_configuration() {
    let root = tempfile::tempdir().unwrap();
    let mut cli = cli(root.path());
    let settings = Settings::load(&cli).unwrap();
    assert_eq!(settings.ui.bottom_margin, 1);
    assert!(!settings.ui.welcome.as_ref().unwrap().wordmark.is_empty());
    cli.no_logo = true;
    let settings = Settings::load(&cli).unwrap();
    let home = settings.ui.welcome.unwrap();
    assert!(home.logo.is_empty() && home.wordmark.is_empty());
    cli.no_logo = false;
    let config = root.path().join("appearance.toml");
    std::fs::write(
        &config,
        "[ui]\nname = 'Example App'\nlogo = ['custom']\nbottom_margin = 2\n",
    )
    .unwrap();
    cli.config = Some(config.clone());
    let settings = Settings::load(&cli).unwrap();
    let home = settings.ui.welcome.unwrap();
    assert_eq!(home.logo, ["custom"]);
    assert!(
        home.wordmark.is_empty(),
        "Custom brands must not inherit RoomKit lettering"
    );
    assert_eq!(settings.ui.bottom_margin, 2);
    std::fs::write(&config, "[ui]\nwelcome = false\nbottom_margin = 0\n").unwrap();
    let settings = Settings::load(&cli).unwrap();
    assert!(settings.ui.welcome.is_none());
    assert_eq!(settings.ui.bottom_margin, 0);
}
