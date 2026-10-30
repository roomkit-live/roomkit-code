//! RoomKit Code's terminal artwork; reusable layout stays in roomkit-tui.

pub(crate) fn welcome_logo() -> Vec<String> {
    [
        "╭─────────╮",
        "│ ███ ███ │",
        "│         │",
        "│ ███ ███ │",
        "╰─────────╯",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

pub(crate) fn welcome_wordmark() -> Vec<String> {
    [
        "                        █       ▀     █",
        "█▄▀▀▄ ▄▀▀▀▄ ▄▀▀▀▄ █▀▄▀▄ █ ▄▀   ▀█    ▀█▀",
        "█     █   █ █   █ █ █ █ █▀▄     █     █     ▄▄",
        "▀      ▀▀▀   ▀▀▀  ▀ ▀ ▀ ▀  ▀   ▀▀▀     ▀▀   ▀▀",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}
