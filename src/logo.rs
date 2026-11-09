//! RoomKit's mark: four blocks in the website's violets, as the RoomKit
//! console draws it (`roomkit.console._brand`).

use roomkit_tui::{Color, LogoTile};

const PRIMARY: Color = Color::rgb(99, 102, 241);
const LIGHT: Color = Color::rgb(129, 140, 248);
const DIM: Color = Color::rgb(55, 58, 130);

/// The mark's two rows of blocks of `block` columns, `gap` columns apart,
/// each row drawn `height` times, `between` empty rows between the two.
fn mark(block: &str, gap: &str, height: usize, between: usize) -> Vec<Vec<LogoTile>> {
    let row = |left: Color, right: Color| {
        vec![
            LogoTile::colored(block, left),
            LogoTile::gap(gap),
            LogoTile::colored(block, right),
        ]
    };
    let top = std::iter::repeat_with(|| row(PRIMARY, LIGHT)).take(height);
    let space = std::iter::repeat_with(|| vec![LogoTile::gap("")]).take(between);
    let bottom = std::iter::repeat_with(|| row(LIGHT, DIM)).take(height);
    top.chain(space).chain(bottom).collect()
}

/// The mark beside the application's name.
pub(crate) fn header_tiles() -> Vec<Vec<LogoTile>> {
    mark("██", " ", 1, 0)
}

/// The mark beside the wordmark of the welcome screen, as tall as it so that
/// they share their top and their baseline.
pub(crate) fn welcome_tiles() -> Vec<Vec<LogoTile>> {
    mark("████", "  ", 2, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(rows: &[Vec<LogoTile>]) -> Vec<String> {
        rows.iter()
            .map(|row| row.iter().map(|tile| tile.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn the_mark_is_four_blocks_in_three_violets() {
        let header = header_tiles();
        assert_eq!(text(&header), ["██ ██", "██ ██"]);
        assert_eq!(header[0][0].color, Some(PRIMARY));
        assert_eq!(header[0][2].color, Some(LIGHT));
        assert_eq!(header[1][0].color, Some(LIGHT));
        assert_eq!(header[1][2].color, Some(DIM));
    }

    #[test]
    fn the_welcome_mark_is_as_tall_as_the_wordmark_beside_it() {
        let welcome = welcome_tiles();
        assert_eq!(welcome.len(), crate::branding::welcome_wordmark().len());
        let drawn: Vec<_> = text(&welcome);
        assert_eq!(drawn[0], "████  ████");
        assert_eq!(drawn[2], "");
        assert_eq!(drawn[4], "████  ████");
    }
}
