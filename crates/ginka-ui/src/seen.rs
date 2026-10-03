//! Which finished conversations the reader has not looked at yet —
//! MonoCode's "done, unseen" mark on a tab and a sidebar row.
//!
//! Window state, not daemon state: what one window has shown its reader says
//! nothing about another window. A row is unseen when its latest session
//! ended after the last time this window showed it. Everything already
//! listed when the window opens counts as seen, so a restart does not light
//! up every row.

use crate::workspace::SessionRow;
use ginka_protocol::WorkspaceId;
use ginka_protocol::model::SessionState;
use std::collections::HashMap;

/// When each workspace's conversation was last on screen, in the session's
/// own `updated_at` clock.
#[derive(Debug, Default, Clone)]
pub struct Seen {
    seen_at: HashMap<WorkspaceId, i64>,
}

impl Seen {
    /// Take in a fresh listing. A workspace seen for the first time is
    /// recorded as seen as it stands; one already known keeps its mark.
    pub fn observe(&mut self, rows: &[SessionRow]) {
        for row in rows {
            self.seen_at
                .entry(row.workspace.clone())
                .or_insert(row.updated_at.unwrap_or_default());
        }
    }

    /// The reader is looking at `row` now.
    pub fn mark_seen(&mut self, row: &SessionRow) {
        self.seen_at
            .insert(row.workspace.clone(), row.updated_at.unwrap_or_default());
    }

    /// Whether `row` ended something the reader has not seen: its session
    /// finished or failed after the last time it was shown, and it is not
    /// the one on screen.
    pub fn is_unseen(&self, row: &SessionRow, showing: Option<&WorkspaceId>) -> bool {
        if showing == Some(&row.workspace) {
            return false;
        }
        let ended = matches!(
            row.session_state,
            Some(SessionState::Finished | SessionState::Failed | SessionState::AwaitingInput)
        );
        let at = row.updated_at.unwrap_or_default();
        ended
            && self
                .seen_at
                .get(&row.workspace)
                .is_some_and(|seen| at > *seen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, state: SessionState, updated_at: i64) -> SessionRow {
        let mut row = SessionRow::samples().remove(0);
        row.workspace = WorkspaceId(format!("comet/{name}"));
        row.session_state = Some(state);
        row.updated_at = Some(updated_at);
        row
    }

    #[test]
    fn what_was_listed_when_the_window_opened_is_not_news() {
        let mut seen = Seen::default();
        let rows = [row("a", SessionState::Finished, 100)];
        seen.observe(&rows);
        assert!(!seen.is_unseen(&rows[0], None));
    }

    #[test]
    fn a_turn_that_ends_while_the_reader_is_elsewhere_is_unseen_until_shown() {
        let mut seen = Seen::default();
        seen.observe(&[row("a", SessionState::Running, 100)]);

        let finished = row("a", SessionState::Finished, 160);
        seen.observe(std::slice::from_ref(&finished));
        assert!(seen.is_unseen(&finished, None));
        assert!(
            !seen.is_unseen(&finished, Some(&finished.workspace)),
            "the one on screen is being seen"
        );

        seen.mark_seen(&finished);
        assert!(!seen.is_unseen(&finished, None));

        let failed = row("a", SessionState::Failed, 200);
        assert!(
            seen.is_unseen(&failed, None),
            "a later failure is news again"
        );
    }

    #[test]
    fn a_running_agent_is_not_unseen_however_new() {
        let mut seen = Seen::default();
        seen.observe(&[row("a", SessionState::Idle, 100)]);
        assert!(!seen.is_unseen(&row("a", SessionState::Running, 500), None));
    }
}
