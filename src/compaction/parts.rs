//! The parts a summary reads a conversation's lines in, one request each:
//! as many as the lines need, up to `MOST`, past which the longer lines are
//! cut shorter instead, then more parts if those still do not fit. A history far past the budget, after `/model` moved it
//! to a small window, took one request per budget's worth of lines, each
//! waiting for the one before.

/// The most requests a summary is written in, unless its lines, cut to
/// `SHORTEST`, still need more.
pub(super) const MOST: usize = 4;
/// The fewest bytes a line is cut to, which still say what it was about.
const SHORTEST: usize = 200;
/// What ends a line cut shorter.
const MORE: &str = " […]";

/// `lines`, oldest first, in parts of `room` bytes each, a line at least in
/// each part: `MOST` parts at most, each line cut to the longest length that
/// allows, but never below `SHORTEST` bytes. One part, empty, for no lines.
pub(super) fn parts(lines: Vec<String>, room: usize) -> Vec<Vec<String>> {
    let sizes: Vec<usize> = lines.iter().map(String::len).collect();
    let cap = cap(&sizes, room);
    let lines: Vec<String> = lines.into_iter().map(|line| cut(line, cap)).collect();
    let placed = placed(lines.iter().map(String::len), room);
    let mut parts: Vec<Vec<String>> = vec![Vec::new()];
    for (line, part) in lines.into_iter().zip(placed) {
        if part == parts.len() {
            parts.push(Vec::new());
        }
        if let Some(current) = parts.last_mut() {
            current.push(line);
        }
    }
    parts
}

/// The longest length lines of `sizes` are cut to so that they take `MOST`
/// parts of `room` at most; `SHORTEST` when even that takes more. Lines cut
/// shorter never take more parts: each part starts at the same line as
/// before, or a later one.
fn cap(sizes: &[usize], room: usize) -> usize {
    let longest = sizes.iter().copied().max().unwrap_or(0);
    let fits = |cap: usize| {
        let placed = placed(sizes.iter().map(|size| (*size).min(cap)), room);
        placed.last().map_or(0, |part| part + 1) <= MOST
    };
    let (mut low, mut high) = (SHORTEST.min(longest), longest);
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if fits(mid) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    low
}

/// The part each line of `sizes` bytes falls in, parts of `room` bytes: a
/// line, and the line break after it, opens the next part when the current
/// one has no room left for them.
fn placed(sizes: impl IntoIterator<Item = usize>, room: usize) -> Vec<usize> {
    let (mut part, mut used) = (0, 0);
    sizes
        .into_iter()
        .map(|size| {
            if used > 0 && used + size + 1 > room {
                part += 1;
                used = 0;
            }
            used += size + 1;
            part
        })
        .collect()
}

/// `line` within `cap` bytes, its end replaced by `MORE` when cut.
fn cut(line: String, cap: usize) -> String {
    if line.len() <= cap {
        return line;
    }
    let kept = line.floor_char_boundary(cap.saturating_sub(MORE.len()));
    format!("{}{MORE}", &line[..kept])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(sizes: &[usize]) -> Vec<String> {
        sizes.iter().map(|size| "x".repeat(*size)).collect()
    }

    fn sizes(parts: &[Vec<String>]) -> Vec<Vec<usize>> {
        parts
            .iter()
            .map(|part| part.iter().map(String::len).collect())
            .collect()
    }

    #[test]
    fn lines_go_in_parts_of_the_room_oldest_first_and_one_at_least() {
        assert_eq!(sizes(&parts(lines(&[10, 10, 10]), 100)), [vec![10, 10, 10]]);
        // A line and its line break past the room open the next part.
        assert_eq!(
            sizes(&parts(lines(&[10, 10, 10]), 22)),
            [vec![10, 10], vec![10]]
        );
        // A line longer than the room takes a part of its own.
        assert_eq!(sizes(&parts(lines(&[50, 10]), 22)), [vec![50], vec![10]]);
        assert_eq!(parts(Vec::new(), 100), [Vec::<String>::new()]);
    }

    /// Past `MOST` parts, every line is cut to the longest length that
    /// fits them, a short line kept whole; never below `SHORTEST`.
    #[test]
    fn lines_past_the_most_parts_are_cut_to_fit_them() {
        let room = 1_000;
        let many = lines(&[900, 900, 900, 900, 900, 900, 100]);
        let cut = parts(many, room);
        assert_eq!(cut.len(), MOST);
        let flat: Vec<&String> = cut.iter().flatten().collect();
        assert_eq!(flat.len(), 7);
        assert!(
            flat[..6]
                .iter()
                .all(|line| line.ends_with(MORE) && line.len() <= 900)
        );
        assert_eq!(flat[6].len(), 100);
        // The longest cut that fits: one byte more takes a part more.
        let longest = flat[0].len();
        let wider = placed([longest + 1; 6].into_iter().chain([100]), room);
        assert!(wider.last().map_or(0, |part| part + 1) > MOST);

        let endless = lines(&[900; 40]);
        let floor = parts(endless, room);
        assert!(floor.len() > MOST);
        assert!(floor.iter().flatten().all(|line| line.len() <= SHORTEST));
    }

    #[test]
    fn a_line_is_cut_on_a_character_boundary() {
        let line = format!("user: {}", "é".repeat(300));
        let cut = cut(line, SHORTEST);
        assert!(cut.len() <= SHORTEST && cut.ends_with("é […]"), "{cut}");
    }
}
