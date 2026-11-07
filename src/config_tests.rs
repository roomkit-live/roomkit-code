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
fn required_intents_default_on_and_cli_overrides_survive_resume() {
    let root = tempfile::tempdir().unwrap();
    let mut cli = cli(root.path());
    assert!(Settings::load(&cli).unwrap().require_tool_intent);
    let config = root.path().join("options.toml");
    std::fs::write(&config, "[agent]\n").unwrap();
    cli.config = Some(config.clone());
    assert!(Settings::load(&cli).unwrap().require_tool_intent);
    std::fs::write(&config, "[agent]\nrequire_tool_intent = false\n").unwrap();
    assert!(!Settings::load(&cli).unwrap().require_tool_intent);
    for required in [false, true] {
        std::fs::write(
            &config,
            format!("[agent]\nrequire_tool_intent = {}\n", !required),
        )
        .unwrap();
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
    assert!(loaded.ui.highlight_own_messages);
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
    std::fs::write(
        cli.config.as_ref().unwrap(),
        "[ui]\nhighlight_own_messages = false\n",
    )
    .unwrap();
    assert!(!Settings::load(&cli).unwrap().ui.highlight_own_messages);
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
    cli.mode = None;
    std::fs::write(&path, "[permissions]\nmode = 'plan'\n").unwrap();
    assert_eq!(Settings::load(&cli).unwrap().mode, Mode::Plan);
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

/// No output limit unless one is configured; the flag wins over the file, and
/// a limit of zero is refused, never read as "no limit".
#[test]
fn the_output_limit_is_optional_and_positive() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.toml");
    std::fs::write(&path, "[provider]\nmax_output_tokens = 4096\n").unwrap();
    let mut cli = cli(root.path());
    assert_eq!(Settings::load(&cli).unwrap().max_output_tokens, None);
    cli.config = Some(path.clone());
    assert_eq!(Settings::load(&cli).unwrap().max_output_tokens, Some(4096));
    cli.max_output_tokens = Some(8192);
    assert_eq!(Settings::load(&cli).unwrap().max_output_tokens, Some(8192));
    cli.max_output_tokens = Some(0);
    let error = Settings::load(&cli).err().unwrap();
    assert_eq!(
        error.to_string(),
        "max output tokens must be a positive token count"
    );
    cli.max_output_tokens = None;
    std::fs::write(&path, "[provider]\nmax_output_tokens = 0\n").unwrap();
    assert!(Settings::load(&cli).is_err());
    assert!(Cli::try_parse_from(["roomkit-code", "--max-output-tokens", "-1"]).is_err());
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

#[test]
fn update_checks_can_be_disabled_and_that_choice_survives_resume() {
    let root = tempfile::tempdir().unwrap();
    let mut args = cli(root.path());
    assert!(Settings::load(&args).unwrap().update_check);
    args.no_update_check = true;
    assert!(!Settings::load(&args).unwrap().update_check);
    assert!(
        resume_options(&args)
            .unwrap()
            .unwrap()
            .contains(&"--no-update-check".into())
    );
    let file = root.path().join("options.toml");
    std::fs::write(&file, "[updates]\ncheck = false\n").unwrap();
    args.config = Some(file);
    args.no_update_check = false;
    assert!(!Settings::load(&args).unwrap().update_check);
    assert!(Cli::try_parse_from(["roomkit-code", "--self-update", "--rollback"]).is_err());
}

#[test]
fn a_missing_workspace_is_named() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing-project");
    let error = Settings::load(&cli(&missing)).err().unwrap().to_string();
    assert!(error.contains("missing-project"), "{error}");
}

#[test]
fn added_shell_variables_come_from_the_flag_the_variable_or_the_file() {
    let names = |list: &[&str]| {
        list.iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>()
    };
    let file = names(&["LD_LIBRARY_PATH", "NIX_*"]);
    // The flag replaces the variable, which replaces the file.
    assert_eq!(choose_inherit_env(&[], None, file.clone()).unwrap(), file);
    assert_eq!(
        choose_inherit_env(&[], Some("CONDA_*, PKG_CONFIG_PATH,".into()), file.clone()).unwrap(),
        names(&["CONDA_*", "PKG_CONFIG_PATH"])
    );
    assert_eq!(
        choose_inherit_env(&names(&["CC"]), Some("CONDA_*".into()), file.clone()).unwrap(),
        names(&["CC"])
    );
    // An empty variable or entry is unset, not an error.
    assert_eq!(
        choose_inherit_env(&[], Some(String::new()), file.clone()).unwrap(),
        file
    );
    // A lone `*` would pass every variable, keys included, from any source.
    assert!(choose_inherit_env(&names(&["*"]), None, vec![]).is_err());
    assert!(choose_inherit_env(&[], Some("*".into()), vec![]).is_err());
    assert!(choose_inherit_env(&names(&["CC"]), None, names(&["*"])).is_err());
}

#[test]
fn the_file_list_loads_and_a_resume_replays_only_the_flag() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("shell.toml");
    std::fs::write(
        &config,
        "[shell]\ninherit_env = ['LD_LIBRARY_PATH', 'NIX_*']\n",
    )
    .unwrap();
    let mut cli = cli(root.path());
    cli.config = Some(config);
    cli.inherit_env = vec!["CC".into()];
    assert_eq!(Settings::load(&cli).unwrap().inherit_env, ["CC"]);
    assert_eq!(
        resume_options(&cli).unwrap().unwrap(),
        [
            "--inherit-env",
            "CC",
            "--config",
            cli.config.as_ref().unwrap().to_str().unwrap()
        ]
    );
}

/// RMK-1461: `[provider.pricing]` states a server's price per million tokens,
/// checked as the kit checks its catalogue's; a wrong or misspelt rate is refused.
#[test]
fn a_configured_price_is_read_and_a_wrong_one_refused() {
    let root = tempfile::tempdir().unwrap();
    let mut cli = cli(root.path());
    let config = root.path().join("options.toml");
    cli.config = Some(config.clone());
    let write =
        |body: &str| std::fs::write(&config, format!("[provider.pricing]\n{body}\n")).unwrap();
    write(
        "input_per_million = 0.5\noutput_per_million = 1.5\ncache_read_per_million = 0.05\nverified = \"2026-11-06\"",
    );
    let pricing = Settings::load(&cli).unwrap().pricing.unwrap();
    assert_eq!(pricing.input_per_million.to_bits(), 0.5_f64.to_bits());
    assert_eq!(pricing.output_per_million.to_bits(), 1.5_f64.to_bits());
    assert_eq!(
        pricing.cache_read_per_million.map(f64::to_bits),
        Some(0.05_f64.to_bits())
    );
    assert_eq!(
        (pricing.cache_write_per_million, pricing.currency.as_str()),
        (None, "USD")
    );
    for wrong in [
        "input_per_million = -1.0\noutput_per_million = 1.0\nverified = \"2026-11-06\"",
        "input_per_million = 1.0\noutput_per_million = 1.0\nverified = \"tomorrow\"",
        "input_per_million = 1.0\noutput_per_million = 1.0\ncache_reads_per_million = 0.1\nverified = \"2026-11-06\"",
        "input_per_million = 1.0\noutput_per_million = 1.0",
    ] {
        write(wrong);
        assert!(Settings::load(&cli).is_err(), "{wrong}");
    }
    std::fs::write(&config, "[provider]\n").unwrap();
    assert!(Settings::load(&cli).unwrap().pricing.is_none());
}

#[test]
fn tool_rounds_default_to_the_engine_s_bound_and_survive_resume() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.toml");
    let mut cli = cli(root.path());
    assert_eq!(Settings::load(&cli).unwrap().max_tool_rounds, 50);
    std::fs::write(&path, "[agent]\nmax_tool_rounds = 200\n").unwrap();
    cli.config = Some(path.clone());
    assert_eq!(Settings::load(&cli).unwrap().max_tool_rounds, 200);
    cli.max_tool_rounds = Some(400);
    assert_eq!(Settings::load(&cli).unwrap().max_tool_rounds, 400);
    // The flag reopens the session with the same bound; the file's is reread.
    let options = resume_options(&cli).unwrap().unwrap();
    assert!(
        options
            .windows(2)
            .any(|pair| pair == ["--max-tool-rounds", "400"])
    );
    cli.max_tool_rounds = Some(0);
    let error = Settings::load(&cli).err().unwrap();
    assert_eq!(error.to_string(), "max tool rounds must be at least 1");
    cli.max_tool_rounds = None;
    std::fs::write(&path, "[agent]\nmax_tool_rounds = 0\n").unwrap();
    assert!(Settings::load(&cli).is_err());
}

#[test]
fn a_prompt_makes_the_launch_unattended_and_json_needs_one() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().to_str().unwrap();
    let base = [
        "roomkit-code",
        workspace,
        "--base-url",
        "http://localhost:8000/v1",
        "--model",
        "m",
    ];
    let parsed = Cli::try_parse_from(base).unwrap();
    assert!(!Settings::load(&parsed).unwrap().unattended);
    let parsed = Cli::try_parse_from(base.into_iter().chain(["-p", "Fix it.", "--json"])).unwrap();
    assert_eq!(parsed.prompt.as_deref(), Some("Fix it."));
    assert!(parsed.json);
    let settings = Settings::load(&parsed).unwrap();
    assert!(settings.unattended);
    // Nobody can say to continue: the turn runs until it answers, unless
    // the launch sets a bound.
    assert_eq!(settings.max_tool_rounds, u32::MAX);
    let bounded = base
        .into_iter()
        .chain(["-p", "Fix it.", "--max-tool-rounds", "80"]);
    let parsed_bounded = Cli::try_parse_from(bounded).unwrap();
    assert_eq!(Settings::load(&parsed_bounded).unwrap().max_tool_rounds, 80);
    // A resumed session opens in the terminal: the message is not replayed.
    assert!(
        !resume_options(&parsed)
            .unwrap()
            .unwrap()
            .iter()
            .any(|arg| arg.contains("Fix it") || arg == "--json" || arg == "-p")
    );
    assert!(Cli::try_parse_from(base.into_iter().chain(["--json"])).is_err());
    assert!(Cli::try_parse_from(base.into_iter().chain(["-p", "x", "--check-config"])).is_err());
}

#[test]
fn the_first_agent_s_reasoning_effort_is_explicit_and_checked() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("settings.toml");
    let mut cli = cli(root.path());
    assert_eq!(Settings::load(&cli).unwrap().reasoning_effort, None);
    std::fs::write(&path, "[provider]\nreasoning_effort = \"high\"\n").unwrap();
    cli.config = Some(path.clone());
    assert_eq!(
        Settings::load(&cli).unwrap().reasoning_effort.as_deref(),
        Some("high")
    );
    cli.reasoning_effort = Some("low".into());
    assert_eq!(
        Settings::load(&cli).unwrap().reasoning_effort.as_deref(),
        Some("low")
    );
    cli.reasoning_effort = None;
    std::fs::write(&path, "[provider]\nreasoning_effort = \"turbo\"\n").unwrap();
    let error = Settings::load(&cli).err().unwrap();
    assert_eq!(
        error.to_string(),
        "reasoning effort must be low, medium, high, xhigh or max"
    );
    let workspace = root.path().to_str().unwrap();
    assert!(
        Cli::try_parse_from(["roomkit-code", workspace, "--reasoning-effort", "turbo"]).is_err()
    );
    assert!(Cli::try_parse_from(["roomkit-code", workspace, "--reasoning-effort", "max"]).is_ok());
}
