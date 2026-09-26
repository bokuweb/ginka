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

/// How long a dialog, a menu or a card takes to appear.
pub const APPEAR: Duration = Duration::from_millis(140);

/// Fade `element` in as it first appears. Played again whenever it is drawn
/// afresh, which for a dialog is each time it opens.
pub fn fade_in<E>(id: impl Into<gpui::ElementId>, element: E) -> gpui::AnimationElement<E>
where
    E: gpui::IntoElement + gpui::Styled + 'static,
{
    use gpui::AnimationExt as _;
    element.with_animation(
        id,
        gpui::Animation::new(APPEAR).with_easing(ease_out),
        |element, delta| element.opacity(delta),
    )
}

/// Fade `element` in and let it rise the last few pixels into place — for a
/// menu or card that opens from something below it.
pub fn rise_in<E>(id: impl Into<gpui::ElementId>, element: E) -> gpui::AnimationElement<E>
where
    E: gpui::IntoElement + gpui::Styled + 'static,
{
    use gpui::AnimationExt as _;
    element.with_animation(
        id,
        gpui::Animation::new(APPEAR).with_easing(ease_out),
        |element, delta| element.opacity(delta).mt(gpui::px(4.0 * (1.0 - delta))),
    )
}

/// How long a panel takes to come in or go.
pub const PANEL: Duration = Duration::from_millis(200);

/// A panel on its way in or out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    /// Coming in rather than going.
    pub opening: bool,
    /// When it started.
    pub started: std::time::Instant,
    /// Distinct per transition, so an animation keyed by it starts afresh
    /// when the panel is toggled again mid-way.
    pub id: u64,
}

/// The transitions under way, one per thing that can come and go.
#[derive(Debug)]
pub struct Transitions<K> {
    running: HashMap<K, Transition>,
    next: u64,
}

impl<K> Default for Transitions<K> {
    fn default() -> Self {
        Self {
            running: HashMap::new(),
            next: 0,
        }
    }
}

impl<K: Eq + Hash + Clone> Transitions<K> {
    /// Start `key` coming in or going, replacing whatever it was doing.
    pub fn start(&mut self, key: K, opening: bool, now: std::time::Instant) -> u64 {
        self.next += 1;
        self.running.insert(
            key,
            Transition {
                opening,
                started: now,
                id: self.next,
            },
        );
        self.next
    }

    /// The transition `key` is in, while it lasts.
    pub fn current(&self, key: &K, now: std::time::Instant) -> Option<Transition> {
        self.running
            .get(key)
            .filter(|transition| now.saturating_duration_since(transition.started) <= PANEL)
            .copied()
    }

    /// Whether `key` is still on its way out, and so still drawn.
    pub fn closing(&self, key: &K, now: std::time::Instant) -> bool {
        self.current(key, now)
            .is_some_and(|transition| !transition.opening)
    }

    /// Forget the transitions that have finished.
    pub fn prune(&mut self, now: std::time::Instant) {
        self.running
            .retain(|_, transition| now.saturating_duration_since(transition.started) <= PANEL);
    }
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

    #[test]
    fn a_panel_is_drawn_while_it_goes_and_not_after() {
        let start = std::time::Instant::now();
        let mut motion = Transitions::default();
        let first = motion.start("right", false, start);
        assert!(motion.closing(&"right", start + PANEL / 2));
        assert!(!motion.closing(&"right", start + PANEL + Duration::from_millis(1)));
        assert_eq!(motion.current(&"right", start).map(|t| t.id), Some(first));

        // Toggled back mid-way: it is opening now, under a new id.
        let second = motion.start("right", true, start + PANEL / 4);
        assert_ne!(first, second);
        assert!(!motion.closing(&"right", start + PANEL / 2));
        let now = motion.current(&"right", start + PANEL / 2).unwrap();
        assert!(now.opening);

        motion.prune(start + PANEL * 2);
        assert_eq!(motion.current(&"right", start + PANEL * 2), None);
        assert!(!motion.closing(&"other", start));
    }
}
