//! The git graph's lanes.
//!
//! Each commit is a dot in a lane, and each row says which lanes pass through
//! it and where its parents continue. Worked out here, from the commits alone,
//! so the view only has to draw what a row says: a dot, the lines through it,
//! and the curves out to its parents.
//!
//! The rule is the usual one. A lane is a slot waiting for one commit — the
//! parent of something already drawn. A commit takes the lane waiting for it
//! (the leftmost, if several are), its first parent inherits that lane, and
//! every further parent takes a free lane of its own. Lanes that were also
//! waiting for this commit end here: that is a branch point.

use ginka_protocol::model::GitCommit;

/// One row of the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRow {
    /// The lane the commit's dot is in.
    pub lane: usize,
    /// Lanes that run straight through this row without touching the commit.
    pub through: Vec<usize>,
    /// Lanes that end at this commit from above — itself included, unless it
    /// is a tip nothing above was waiting for.
    pub into: Vec<usize>,
    /// Lanes the commit's parents continue in below, first parent first.
    pub out: Vec<usize>,
    /// How many lanes are in use at this row, for its width.
    pub width: usize,
}

/// Lay out the lanes for commits in topological order, newest first.
pub fn layout(commits: &[GitCommit]) -> Vec<GraphRow> {
    // Each slot holds the commit it is waiting for.
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());

    for commit in commits {
        let waiting: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.as_deref() == Some(commit.id.as_str()))
            .map(|(index, _)| index)
            .collect();
        let lane = match waiting.first() {
            Some(lane) => *lane,
            // A tip nothing above leads to: the first free slot, or a new one.
            None => free_slot(&mut lanes),
        };
        let through: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(index, slot)| slot.is_some() && !waiting.contains(index) && *index != lane)
            .map(|(index, _)| index)
            .collect();

        // Every lane that was waiting for this commit is done with it.
        for index in &waiting {
            lanes[*index] = None;
        }

        let mut out = Vec::with_capacity(commit.parents.len());
        for (position, parent) in commit.parents.iter().enumerate() {
            // A parent some other lane is already heading for joins it rather
            // than opening a second line to the same commit.
            if let Some(existing) = lanes
                .iter()
                .position(|slot| slot.as_deref() == Some(parent.as_str()))
            {
                out.push(existing);
                continue;
            }
            let slot = if position == 0 {
                lane
            } else {
                free_slot(&mut lanes)
            };
            lanes[slot] = Some(parent.clone());
            out.push(slot);
        }

        // Trailing empty lanes are dropped, so a graph narrows again once a
        // branch has merged back.
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }

        let width = through
            .iter()
            .chain(&waiting)
            .chain(&out)
            .chain(std::iter::once(&lane))
            .max()
            .map(|max| max + 1)
            .unwrap_or(1);
        rows.push(GraphRow {
            lane,
            through,
            into: waiting,
            out,
            width,
        });
    }
    rows
}

/// The first empty slot, making one if every slot is taken.
fn free_slot(lanes: &mut Vec<Option<String>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(index) => index,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(id: &str, parents: &[&str]) -> GitCommit {
        GitCommit {
            id: id.into(),
            parents: parents.iter().map(|parent| parent.to_string()).collect(),
            author: String::new(),
            authored_at: 0,
            summary: String::new(),
        }
    }

    #[test]
    fn a_straight_history_stays_in_one_lane() {
        let rows = layout(&[commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])]);
        assert!(rows.iter().all(|row| row.lane == 0 && row.width == 1));
        assert_eq!(rows[0].into, Vec::<usize>::new(), "the tip starts here");
        assert_eq!(rows[1].into, vec![0]);
        assert_eq!(rows[2].out, Vec::<usize>::new(), "a root ends the lane");
    }

    #[test]
    fn a_merge_opens_a_lane_that_closes_at_the_branch_point() {
        //   m        merge of b and x
        //   |\\
        //   b |
        //   | x
        //   |/
        //   a
        let rows = layout(&[
            commit("m", &["b", "x"]),
            commit("b", &["a"]),
            commit("x", &["a"]),
            commit("a", &[]),
        ]);
        assert_eq!(rows[0].out, vec![0, 1]);
        assert_eq!(rows[1].lane, 0);
        assert_eq!(rows[1].through, vec![1]);
        assert_eq!(rows[2].lane, 1);
        assert_eq!(rows[2].through, vec![0]);
        // x heads for a, which lane 0 is already waiting for: it joins.
        assert_eq!(rows[2].out, vec![0]);
        assert_eq!(rows[3].lane, 0);
        assert_eq!(rows[3].width, 1, "the graph narrows once merged");
    }

    #[test]
    fn two_tips_sit_side_by_side_until_they_meet() {
        // Two branch heads over a shared base, neither above the other.
        let rows = layout(&[
            commit("h1", &["base"]),
            commit("h2", &["base"]),
            commit("base", &[]),
        ]);
        assert_eq!(rows[0].lane, 0);
        assert_eq!(rows[1].lane, 1, "a second tip gets a lane of its own");
        assert_eq!(rows[1].out, vec![0], "and joins the lane going to the base");
        assert_eq!(rows[2].into, vec![0]);
    }

    #[test]
    fn a_lane_freed_by_a_merge_is_reused() {
        let rows = layout(&[
            commit("m", &["a", "x"]),
            commit("x", &["a"]),
            commit("a", &["z"]),
            commit("t", &["z"]),
            commit("z", &[]),
        ]);
        // After m's second parent joined back, lane 1 is free for the tip t.
        assert_eq!(rows[3].lane, 1);
    }
}
