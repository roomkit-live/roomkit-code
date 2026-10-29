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
