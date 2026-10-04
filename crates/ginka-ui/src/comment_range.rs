//! Which lines a review comment covers while it is being written — Orca's
//! multi-line comments, picked with a click and a shift-click.

/// The range after a shift-click on `clicked`, given the comment was opened
/// on `start`: from the lower line to the higher, whichever way the reader
/// dragged, and a single line again when they click where it began.
pub fn extend(start: u32, clicked: u32) -> (u32, Option<u32>) {
    let (low, high) = if clicked < start {
        (clicked, start)
    } else {
        (start, clicked)
    };
    (low, (high > low).then_some(high))
}

/// How a range is written beside the comment box: `12–18`, or `12`.
pub fn label(start: u32, end: Option<u32>) -> String {
    match end {
        Some(end) => format!("{start}–{end}"),
        None => start.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shift_click_reaches_either_way_and_back_to_one_line() {
        assert_eq!(extend(12, 18), (12, Some(18)));
        assert_eq!(extend(18, 12), (12, Some(18)), "upwards reads the same");
        assert_eq!(extend(12, 12), (12, None));
        assert_eq!(label(12, Some(18)), "12–18");
        assert_eq!(label(7, None), "7");
    }
}
