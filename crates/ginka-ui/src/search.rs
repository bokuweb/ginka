//! View-state decisions for searching: the open conversation, and
//! everything at once.

use ginka_protocol::SessionId;
use ginka_protocol::model::{SessionMatch, WorkspaceContentMatch, WorkspaceFileMatch};

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

/// One result of searching everywhere.
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    /// A conversation line the query appears in.
    Conversation(SessionMatch),
    /// A file whose path matches.
    File(WorkspaceFileMatch),
    /// A line of a file the query appears in.
    Line(WorkspaceContentMatch),
}

/// How many results of each kind searching everywhere shows.
pub const PER_GROUP: usize = 8;

/// Search results as one list to move through: conversations first — the
/// most recent, and at most two lines of any one conversation — then file
/// paths, then file lines, each kind capped at `per_group`.
pub fn everywhere(
    conversations: Vec<SessionMatch>,
    files: Vec<WorkspaceFileMatch>,
    lines: Vec<WorkspaceContentMatch>,
    per_group: usize,
) -> Vec<Found> {
    let mut conversations = conversations;
    conversations.sort_by_key(|found| std::cmp::Reverse(found.at));
    let mut per_session: std::collections::HashMap<SessionId, usize> = Default::default();
    let conversations = conversations
        .into_iter()
        .filter(|found| {
            let seen = per_session.entry(found.session.clone()).or_default();
            *seen += 1;
            *seen <= 2
        })
        .take(per_group)
        .map(Found::Conversation);
    conversations
        .chain(files.into_iter().take(per_group).map(Found::File))
        .chain(lines.into_iter().take(per_group).map(Found::Line))
        .collect()
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

    fn hit(session: &str, seq: u64, at: i64) -> SessionMatch {
        SessionMatch {
            session: SessionId(session.into()),
            workspace: ginka_protocol::WorkspaceId(format!("comet/{session}")),
            title: None,
            seq,
            at,
            excerpt: format!("{session} {seq}"),
        }
    }

    #[test]
    fn everything_found_is_one_list_newest_conversation_first() {
        let file = |path: &str| WorkspaceFileMatch {
            workspace: ginka_protocol::WorkspaceId("comet/main".into()),
            path: path.into(),
        };
        let line = |line: u32| WorkspaceContentMatch {
            workspace: ginka_protocol::WorkspaceId("comet/main".into()),
            path: "src/lib.rs".into(),
            line,
            text: "cache".into(),
        };
        let found = everywhere(
            vec![
                hit("a", 1, 10),
                hit("b", 5, 30),
                hit("a", 2, 20),
                hit("a", 3, 40),
            ],
            vec![file("src/cache.rs"), file("src/lib.rs")],
            vec![line(3), line(9), line(12)],
            2,
        );
        assert_eq!(
            found,
            vec![
                // Newest first, and no more than two of conversation a.
                Found::Conversation(hit("a", 3, 40)),
                Found::Conversation(hit("b", 5, 30)),
                Found::File(file("src/cache.rs")),
                Found::File(file("src/lib.rs")),
                Found::Line(line(3)),
                Found::Line(line(9)),
            ]
        );
    }

    #[test]
    fn a_conversation_gives_at_most_two_lines() {
        let found = everywhere(
            vec![hit("a", 1, 1), hit("a", 2, 2), hit("a", 3, 3)],
            Vec::new(),
            Vec::new(),
            PER_GROUP,
        );
        assert_eq!(
            found,
            vec![
                Found::Conversation(hit("a", 3, 3)),
                Found::Conversation(hit("a", 2, 2)),
            ]
        );
    }
}
