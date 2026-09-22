//! Back and forward history for project and session visits.

/// A bounded browser-style history of visible destinations.
#[derive(Debug, Clone)]
pub struct NavigationHistory<T> {
    visits: Vec<T>,
    current: usize,
}

impl<T: Clone + PartialEq> NavigationHistory<T> {
    /// Maximum destinations retained by one window.
    pub const MAX_VISITS: usize = 100;

    /// Begin with the destination the window initially displays.
    pub fn new(initial: T) -> Self {
        Self {
            visits: vec![initial],
            current: 0,
        }
    }

    /// Return the destination at the current history position.
    pub fn current(&self) -> T {
        self.visits[self.current].clone()
    }

    /// Record an explicit visit and discard any forward branch.
    pub fn visit(&mut self, destination: T) {
        if self.visits.get(self.current) == Some(&destination) {
            return;
        }
        self.visits.truncate(self.current + 1);
        self.visits.push(destination);
        if self.visits.len() > Self::MAX_VISITS {
            let overflow = self.visits.len() - Self::MAX_VISITS;
            self.visits.drain(..overflow);
        }
        self.current = self.visits.len() - 1;
    }

    /// Whether at least one older position exists.
    pub fn can_go_back(&self) -> bool {
        self.current > 0
    }

    /// Whether at least one newer position exists.
    pub fn can_go_forward(&self) -> bool {
        self.current + 1 < self.visits.len()
    }

    /// Whether an older available destination exists.
    pub fn can_go_back_where(&self, mut available: impl FnMut(&T) -> bool) -> bool {
        self.visits[..self.current].iter().rev().any(&mut available)
    }

    /// Whether a newer available destination exists.
    pub fn can_go_forward_where(&self, mut available: impl FnMut(&T) -> bool) -> bool {
        self.visits[self.current + 1..].iter().any(&mut available)
    }

    /// Move to the nearest older destination accepted by `available`.
    ///
    /// Missing destinations are skipped without mutating the stored sequence.
    /// If none is available, the current position is left unchanged.
    pub fn back_where(&mut self, mut available: impl FnMut(&T) -> bool) -> Option<T> {
        let mut candidate = self.current;
        while let Some(previous) = candidate.checked_sub(1) {
            candidate = previous;
            if available(&self.visits[candidate]) {
                self.current = candidate;
                return Some(self.visits[candidate].clone());
            }
        }
        None
    }

    /// Move to the nearest newer destination accepted by `available`.
    ///
    /// Missing destinations are skipped without mutating the stored sequence.
    /// If none is available, the current position is left unchanged.
    pub fn forward_where(&mut self, mut available: impl FnMut(&T) -> bool) -> Option<T> {
        let mut candidate = self.current + 1;
        while candidate < self.visits.len() {
            if available(&self.visits[candidate]) {
                self.current = candidate;
                return Some(self.visits[candidate].clone());
            }
            candidate += 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visits_move_back_and_forward_without_recording_duplicates() {
        let mut history = NavigationHistory::new("home");
        history.visit("project");
        history.visit("project");
        history.visit("session");

        assert_eq!(history.back_where(|_| true), Some("project"));
        assert_eq!(history.back_where(|_| true), Some("home"));
        assert_eq!(history.back_where(|_| true), None);
        assert_eq!(history.forward_where(|_| true), Some("project"));
        assert_eq!(history.forward_where(|_| true), Some("session"));
        assert_eq!(history.forward_where(|_| true), None);
    }

    #[test]
    fn a_new_visit_after_back_discards_the_forward_branch() {
        let mut history = NavigationHistory::new("home");
        history.visit("one");
        history.visit("two");
        assert_eq!(history.back_where(|_| true), Some("one"));

        history.visit("three");

        assert_eq!(history.forward_where(|_| true), None);
        assert_eq!(history.back_where(|_| true), Some("one"));
    }

    #[test]
    fn navigation_skips_targets_that_no_longer_exist() {
        let mut history = NavigationHistory::new("home");
        history.visit("removed-project");
        history.visit("removed-session");
        history.visit("current");

        assert_eq!(
            history.back_where(|target| !target.starts_with("removed")),
            Some("home")
        );
        assert_eq!(
            history.forward_where(|target| !target.starts_with("removed")),
            Some("current")
        );
    }

    #[test]
    fn history_is_bounded_without_losing_the_current_visit() {
        let mut history = NavigationHistory::new(0);
        for visit in 1..=(NavigationHistory::<usize>::MAX_VISITS + 10) {
            history.visit(visit);
        }

        assert_eq!(
            history.current(),
            NavigationHistory::<usize>::MAX_VISITS + 10
        );
        let mut steps = 0;
        while history.back_where(|_| true).is_some() {
            steps += 1;
        }
        assert_eq!(steps, NavigationHistory::<usize>::MAX_VISITS - 1);
    }
}
