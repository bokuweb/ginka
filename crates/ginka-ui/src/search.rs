//! View-state decisions for searching the open conversation.

use ginka_protocol::SessionId;
use ginka_protocol::model::SessionMatch;

/// Which way the result cursor moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Towards later transcript positions.
    Next,
    /// Towards earlier transcript positions.
    Previous,
}

/// Keep only one open conversation's matches and order them as it is read.
pub fn matches_for_session(matches: Vec<SessionMatch>, session: &SessionId) -> Vec<SessionMatch> {
    let mut matches: Vec<_> = matches
        .into_iter()
        .filter(|found| &found.session == session)
        .collect();
    matches.sort_by_key(|found| found.seq);
    matches
}

/// Move a search cursor, wrapping at either edge.
pub fn step(count: usize, current: Option<usize>, direction: Direction) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match (current, direction) {
        (Some(index), Direction::Next) => (index + 1) % count,
        (Some(0), Direction::Previous) | (None, Direction::Previous) => count - 1,
        (Some(index), Direction::Previous) => index.saturating_sub(1),
        (None, Direction::Next) => 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ginka_protocol::model::SessionMatch;
    use ginka_protocol::{SessionId, WorkspaceId};

    fn found(session: &str, seq: u64) -> SessionMatch {
        SessionMatch {
            session: SessionId(session.into()),
            workspace: WorkspaceId("app/main".into()),
            title: None,
            seq,
            at: seq as i64,
            excerpt: format!("match {seq}"),
        }
    }

    #[test]
    fn an_open_conversation_keeps_only_its_matches_in_document_order() {
        let matches = matches_for_session(
            vec![found("other", 2), found("open", 9), found("open", 3)],
            &SessionId("open".into()),
        );
        assert_eq!(
            matches.iter().map(|found| found.seq).collect::<Vec<_>>(),
            vec![3, 9]
        );
    }

    #[test]
    fn next_and_previous_wrap_around_the_result_set() {
        assert_eq!(step(3, Some(2), Direction::Next), Some(0));
        assert_eq!(step(3, Some(0), Direction::Previous), Some(2));
        assert_eq!(step(0, None, Direction::Next), None);
    }
}
