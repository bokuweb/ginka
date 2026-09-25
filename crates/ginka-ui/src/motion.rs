//! Motion of our own (`docs/ui.md` §6): the curve lists move on, and which
//! rows of a reordered list actually moved.
//!
//! When a session jumps to the top because it needs the reader, every row
//! below it shifts down by one — but only the one that jumped moved in any
//! sense the reader cares about. That set is everything outside the longest
//! run the reorder kept in order, which is what this finds; the sidebar then
//! slides only those rows in, over [`REORDER`].

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Duration;

/// How long a reordered row takes to settle.
pub const REORDER: Duration = Duration::from_millis(260);

/// Which way a row went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moved {
    Up,
    Down,
}

/// Ease out: fast at first, settling at the end — a row arrives rather
/// than stops. `t` is clamped to 0..=1.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// The rows of `after` that moved relative to the others since `before`,
/// and which way. Rows that only appeared or disappeared did not move.
pub fn moved<T: Eq + Hash + Clone>(before: &[T], after: &[T]) -> Vec<(T, Moved)> {
    // Only rows on both sides can have moved; rank them among those.
    let after_set: HashMap<&T, ()> = after.iter().map(|row| (row, ())).collect();
    let old_rank: HashMap<&T, usize> = before
        .iter()
        .filter(|row| after_set.contains_key(row))
        .enumerate()
        .map(|(rank, row)| (row, rank))
        .collect();
    let common: Vec<(&T, usize)> = after
        .iter()
        .filter_map(|row| old_rank.get(row).map(|rank| (row, *rank)))
        .collect();

    // The longest run still in the old order stayed; the rest moved.
    let kept = longest_increasing(&common.iter().map(|(_, rank)| *rank).collect::<Vec<_>>());
    common
        .iter()
        .enumerate()
        .filter(|(position, _)| !kept[*position])
        .map(|(position, (row, rank))| {
            let direction = if position < *rank {
                Moved::Up
            } else {
                Moved::Down
            };
            ((*row).clone(), direction)
        })
        .collect()
}

/// Which positions of `values` are in one longest strictly increasing
/// subsequence (patience sorting, O(n log n)).
fn longest_increasing(values: &[usize]) -> Vec<bool> {
    let mut tails: Vec<usize> = Vec::new(); // positions, by the value they end on
    let mut previous: Vec<Option<usize>> = vec![None; values.len()];
    for (position, value) in values.iter().enumerate() {
        let slot = tails.partition_point(|&tail| values[tail] < *value);
        previous[position] = slot.checked_sub(1).map(|before| tails[before]);
        if slot == tails.len() {
            tails.push(position);
        } else {
            tails[slot] = position;
        }
    }
    let mut kept = vec![false; values.len()];
    let mut at = tails.last().copied();
    while let Some(position) = at {
        kept[position] = true;
        at = previous[position];
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curve_starts_fast_and_settles() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!(ease_out(0.5) > 0.5, "more than half way at half time");
        assert!(ease_out(0.25) < ease_out(0.5));
        assert_eq!(ease_out(-1.0), 0.0);
        assert_eq!(ease_out(2.0), 1.0);
    }

    #[test]
    fn a_row_that_jumps_to_the_top_is_the_only_one_that_moved() {
        assert_eq!(
            moved(&["a", "b", "c", "d"], &["d", "a", "b", "c"]),
            [("d", Moved::Up)]
        );
        assert_eq!(
            moved(&["a", "b", "c", "d"], &["b", "c", "d", "a"]),
            [("a", Moved::Down)]
        );
    }

    #[test]
    fn nothing_moved_when_the_order_held() {
        assert!(moved(&["a", "b", "c"], &["a", "b", "c"]).is_empty());
        // Rows coming and going shift others without moving them.
        assert!(moved(&["a", "b", "c"], &["a", "x", "c"]).is_empty());
        assert!(moved::<&str>(&[], &["a", "b"]).is_empty());
    }

    #[test]
    fn a_swap_moves_one_of_the_two() {
        let swapped = moved(&["a", "b", "c"], &["a", "c", "b"]);
        assert_eq!(swapped.len(), 1, "{swapped:?}");
    }
}
