//! What the terminal's views look like: the file's `[ui]` and the options
//! that override it.
use roomkit_tui::{
    Branding, ColorMode, SelectionMode, TerminalStatus, Theme, UiOptions, WelcomeScreen,
};

use super::file::Appearance;
use super::{Cli, Color, Palette, Result};

/// The name and the logo the views show, as the file sets them, unless
/// `--no-logo` leaves the logo out.
fn branding(cli: &Cli, name: String, logo: Option<Vec<String>>) -> Branding {
    let branding = Branding::new(name).with_subtitle("Explore, edit and test your project");
    let mut branding = match logo {
        Some(logo) => branding.with_logo(logo),
        None => branding.with_logo_tiles(crate::logo::header_tiles()),
    };
    if cli.no_logo {
        branding.logo.clear();
        branding.logo_tiles.clear();
    }
    branding
}

/// What the terminal's views look like: the file's `[ui]`, with the
/// options that override it for this launch.
pub(super) fn ui_options(cli: &Cli, appearance: Appearance) -> Result<UiOptions> {
    let custom_logo = appearance.logo.is_some();
    let branding = branding(cli, appearance.name, appearance.logo);
    let welcome = appearance.welcome.then(|| {
        let mut welcome = WelcomeScreen::default();
        welcome.width = appearance.welcome_width;
        if !cli.no_logo {
            match appearance.welcome_logo {
                Some(logo) => welcome.logo = logo,
                None if custom_logo => welcome.logo = branding.logo.clone(),
                None => welcome = welcome.with_logo_tiles(crate::logo::welcome_tiles()),
            }
        }
        welcome.wordmark = if cli.no_logo {
            Vec::new()
        } else {
            appearance.welcome_wordmark.unwrap_or_else(|| {
                if branding.name == "RoomKit Code" {
                    crate::branding::welcome_wordmark()
                } else {
                    Vec::new()
                }
            })
        };
        welcome
    });
    // UiOptions is non-exhaustive: start from its defaults, then set each field.
    let mut ui = UiOptions::default();
    ui.welcome = welcome;
    ui.bottom_margin = appearance.bottom_margin;
    ui.sidebar_visible = appearance.sidebar;
    ui.sidebar_width = appearance.sidebar_width;
    ui.terminal_status = (appearance.terminal_status && !cli.no_terminal_status)
        .then(|| TerminalStatus::new("roomkit-code"))
        .transpose()?;
    ui.branding = branding;
    ui.theme = match cli.theme.unwrap_or(appearance.theme) {
        Palette::Dark => Theme::dark(),
        Palette::Light => Theme::light(),
        Palette::Terminal => Theme::terminal(),
    };
    ui.color = match cli.color.unwrap_or(appearance.color) {
        Color::Auto => ColorMode::Auto,
        Color::Always => ColorMode::Always,
        Color::Never => ColorMode::Never,
    };
    ui.animations = appearance.animations && !cli.no_animations;
    ui.markdown = appearance.markdown;
    ui.message_gap = appearance.message_gap;
    ui.highlight_own_messages = appearance.highlight_own_messages;
    ui.tool_preview_lines = appearance.tool_preview_lines;
    ui.tool_activity = appearance.tool_activity;
    ui.input_min_lines = appearance.input_min_lines;
    ui.input_padding = appearance.input_padding;
    ui.input_placeholder = appearance.input_placeholder;
    ui.input_prompt = appearance.input_prompt;
    ui.selection = if cli.native_selection || appearance.native_selection {
        SelectionMode::Native
    } else {
        SelectionMode::CopyOnSelect
    };
    Ok(ui)
}
