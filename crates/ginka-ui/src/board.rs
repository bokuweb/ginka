//! The agents board: every workspace's agent, across projects, sorted into
//! what it needs from the reader — Orca's agent dashboard.
//!
//! The sidebar answers "what is in this project"; the board answers "what
//! is waiting on me, anywhere". It reads the same rows the sidebar does, so
//! it needs nothing from the daemon the sidebar does not already fetch.

use crate::workspace::SessionRow;
use ginka_protocol::model::SessionState;

/// A column of the board, in the order it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Column {
    /// Blocked on the reader: a question, a permission, a failed turn, or a
    /// worktree stopped on conflicts.
    NeedsYou,
    /// An agent is running, or its queue holds work for it.
    Working,
    /// The last turn finished and there is work to look at: changes in the
    /// worktree or commits not yet pushed.
    Review,
    /// Nothing to do: never started, stopped, or finished with nothing left
    /// to review.
    Idle,
}

impl Column {
    /// Every column, in drawing order.
    pub const ALL: [Column; 4] = [Self::NeedsYou, Self::Working, Self::Review, Self::Idle];

    /// The locale key of the column's heading.
    pub fn label_key(self) -> &'static str {
        match self {
            Self::NeedsYou => "board.needs_you",
            Self::Working => "board.working",
            Self::Review => "board.review",
            Self::Idle => "board.idle",
        }
    }

    /// Where `row` belongs.
    ///
    /// A conflict outranks everything, because nothing the agent does next
    /// is right until it is settled; then the agent's own state; and only a
    /// turn that ended leaves the question of whether there is anything to
    /// read.
    pub fn of(row: &SessionRow) -> Self {
        if row.status.conflict {
            return Self::NeedsYou;
        }
        match row.session_state {
            Some(SessionState::AwaitingInput | SessionState::Failed) => Self::NeedsYou,
            Some(SessionState::Starting | SessionState::Running) => Self::Working,
            // A held queue is waiting on a reset or on the reader; either way
            // the work is not done.
            _ if row.queued > 0 => Self::Working,
            Some(SessionState::Finished) if row.status.dirty || row.status.ahead > 0 => {
                Self::Review
            }
            _ => Self::Idle,
        }
    }
}

/// The board's rows, grouped by column in drawing order, archived ones left
/// out and each column keeping the order it was given — the daemon's, which
/// already puts recent work first. `project` narrows it to one project.
pub fn columns<'a>(
    rows: &'a [SessionRow],
    project: Option<&str>,
) -> Vec<(Column, Vec<&'a SessionRow>)> {
    let mut grouped: Vec<(Column, Vec<&SessionRow>)> = Column::ALL
        .iter()
        .map(|column| (*column, Vec::new()))
        .collect();
    for row in rows {
        if row.archived || project.is_some_and(|project| row.origin.as_ref() != project) {
            continue;
        }
        let column = Column::of(row);
        if let Some((_, members)) = grouped.iter_mut().find(|(c, _)| *c == column) {
            members.push(row);
        }
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::SessionRow as Row;

    fn row(state: Option<SessionState>) -> SessionRow {
        let mut row = Row::samples().remove(0);
        row.archived = false;
        row.session_state = state;
        row.status = Default::default();
        row.queued = 0;
        row
    }

    #[test]
    fn each_state_lands_where_the_reader_would_look_for_it() {
        assert_eq!(
            Column::of(&row(Some(SessionState::AwaitingInput))),
            Column::NeedsYou
        );
        assert_eq!(
            Column::of(&row(Some(SessionState::Failed))),
            Column::NeedsYou
        );
        assert_eq!(
            Column::of(&row(Some(SessionState::Running))),
            Column::Working
        );
        assert_eq!(Column::of(&row(Some(SessionState::Finished))), Column::Idle);
        assert_eq!(Column::of(&row(None)), Column::Idle);

        let mut changed = row(Some(SessionState::Finished));
        changed.status.dirty = true;
        assert_eq!(Column::of(&changed), Column::Review);
        let mut unpushed = row(Some(SessionState::Finished));
        unpushed.status.ahead = 2;
        assert_eq!(Column::of(&unpushed), Column::Review);

        let mut held = row(Some(SessionState::Failed));
        held.queued = 1;
        assert_eq!(
            Column::of(&held),
            Column::NeedsYou,
            "a failure is still the reader's"
        );
        let mut waiting = row(Some(SessionState::Cancelled));
        waiting.queued = 1;
        assert_eq!(Column::of(&waiting), Column::Working);

        let mut conflicted = row(Some(SessionState::Running));
        conflicted.status.conflict = true;
        assert_eq!(Column::of(&conflicted), Column::NeedsYou);
    }

    #[test]
    fn the_board_leaves_out_the_archive_and_narrows_to_a_project() {
        let mut rows = vec![
            row(Some(SessionState::Running)),
            row(Some(SessionState::AwaitingInput)),
            row(None),
        ];
        rows[1].origin = "other".into();
        rows[2].archived = true;
        let all = columns(&rows, None);
        assert_eq!(
            all.iter()
                .map(|(c, members)| (*c, members.len()))
                .collect::<Vec<_>>(),
            [
                (Column::NeedsYou, 1),
                (Column::Working, 1),
                (Column::Review, 0),
                (Column::Idle, 0)
            ]
        );
        let origin = rows[0].origin.to_string();
        let one = columns(&rows, Some(&origin));
        assert_eq!(
            one[0].1.len(),
            0,
            "the other project's question is not shown"
        );
        assert_eq!(one[1].1.len(), 1);
    }
}
