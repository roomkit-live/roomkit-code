//! Generated from Space Grotesk 600.
//! Regenerate with scripts/generate-branding.py; layout belongs to roomkit-tui.

pub(crate) fn welcome_wordmark() -> Vec<String> {
    [
        "                                  ▌     ▐█   ▟",
        " ▄▄▄▖ ▗▄▄▄▄    ▄▄▄▄   ▗▗▄▄▄▗▄▄▄   ▌  ▗▖  ▖ ▄▄█▄▄",
        " █▘  ▗▛   ▝▙  ▛    ▜▖ ▐▘  ▐▌  ▐▌  ▌▄▛▘   ▌   █",
        " █   ▐▌    █ ▝▙    ▐▘ ▐   ▐▌  ▐▌  ▛▜▄    ▌   █",
        " █    ▀▄▄▄▟▘  ▀▙▄▄▟▀  ▐   ▐▌  ▐▌  ▌ ▝▜▄  ▌   ▜▄▄  █",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}
