//! The rail beside a history: which lane a commit sits in, and which lanes
//! run past it.
//!
//! A history is a list, but it is not a line: a merge brings two lines
//! together and a branch point sends one out. Drawing that needs to know,
//! for every row, where the dot goes and which columns have a thread
//! running through them. The rule is the one every git viewer uses — a lane
//! is a sha the next row is waiting for — and it is worked out here, once,
//! away from the window, where it can be tested.

/// Half of a row: a thread crosses a row in two pieces, because the dot is
/// in the middle and a thread may bend at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Half {
    /// From the row's top edge to the dot's line.
    Top,
    /// From the dot's line to the row's bottom edge.
    Bottom,
}

/// One piece of thread through a row: it enters at `from` and leaves at
/// `to`, which are the same lane for a thread running straight past.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// The lane it comes in on.
    pub from: usize,
    /// The lane it goes out on.
    pub to: usize,
    /// Which half of the row it crosses.
    pub half: Half,
}

/// Where one commit sits on the rail, and what runs past it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The lane the dot goes in, counting from the left.
    pub lane: usize,
    /// Whether this commit has more than one parent.
    pub merge: bool,
    /// Every piece of thread crossing this row, the ones that bend into
    /// and out of the dot included.
    pub segments: Vec<Segment>,
}

impl Row {
    /// The lanes with a thread in them somewhere in this row.
    pub fn lanes(&self) -> impl Iterator<Item = usize> + '_ {
        self.segments
            .iter()
            .flat_map(|segment| [segment.from, segment.to])
            .chain(std::iter::once(self.lane))
    }
}

/// How many lanes the rail is allowed to grow to. A history wide enough to
/// need more is one nobody reads by its picture, and the rail has to fit
/// beside the message.
pub const MAX_LANES: usize = 6;

/// Lay a history out on the rail.
///
/// `commits` is the list as GitHub sends it, newest first, each with its
/// parents. A lane is a sha that some later row is waiting to see: lanes are
/// claimed as commits appear and freed when nothing is waiting for them, so
/// a linear history stays in lane zero and a merge opens exactly one more.
///
/// What comes back is not "which lanes are busy" but where each thread
/// enters and leaves the row. That is the difference between a rail of
/// disconnected sticks and one where a branch visibly leaves its parent and
/// comes back.
pub fn lay_out<'a, I>(commits: I) -> Vec<Row>
where
    I: IntoIterator<Item = (&'a str, &'a [String])>,
{
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::new();
    for (sha, parents) in commits {
        // What is running as the row begins, before this commit is placed.
        let incoming = lanes.clone();
        let lane = incoming
            .iter()
            .position(|waiting| waiting.as_deref() == Some(sha))
            .or_else(|| lanes.iter().position(Option::is_none))
            .unwrap_or_else(|| {
                lanes.push(None);
                lanes.len() - 1
            });
        let mut segments = Vec::new();
        // The top half: everything arriving. A thread waiting for this
        // commit bends into its dot; the rest run straight past.
        for (index, waiting) in incoming.iter().enumerate() {
            match waiting.as_deref() {
                Some(waited) if waited == sha => segments.push(Segment {
                    from: index,
                    to: lane,
                    half: Half::Top,
                }),
                Some(_) => segments.push(Segment {
                    from: index,
                    to: index,
                    half: Half::Top,
                }),
                None => {}
            }
        }
        // Those threads have met here, so their lanes are free again.
        for (index, waiting) in lanes.iter_mut().enumerate() {
            if index != lane && waiting.as_deref() == Some(sha) {
                *waiting = None;
            }
        }
        // The bottom half: the first parent carries this lane on, and each
        // further parent leaves for a lane of its own.
        lanes[lane] = parents.first().cloned();
        if lanes[lane].is_some() {
            segments.push(Segment {
                from: lane,
                to: lane,
                half: Half::Bottom,
            });
        }
        // A lane opened here has nothing above it, so the bend into it is
        // where its thread starts. A lane that was already running keeps
        // its own line as well: the bend joins that thread, it does not
        // replace it.
        let mut opened: Vec<usize> = Vec::new();
        for parent in parents.iter().skip(1) {
            let existing = lanes
                .iter()
                .position(|waiting| waiting.as_deref() == Some(parent.as_str()));
            let taken = match existing {
                Some(index) => Some(index),
                None => match lanes.iter().position(Option::is_none) {
                    Some(free) => {
                        lanes[free] = Some(parent.clone());
                        opened.push(free);
                        Some(free)
                    }
                    None if lanes.len() < MAX_LANES => {
                        lanes.push(Some(parent.clone()));
                        opened.push(lanes.len() - 1);
                        Some(lanes.len() - 1)
                    }
                    None => None,
                },
            };
            if let Some(index) = taken {
                segments.push(Segment {
                    from: lane,
                    to: index,
                    half: Half::Bottom,
                });
            }
        }
        // Everything else still running crosses the bottom half untouched.
        for (index, waiting) in lanes.iter().enumerate() {
            if index != lane && waiting.is_some() && !opened.contains(&index) {
                segments.push(Segment {
                    from: index,
                    to: index,
                    half: Half::Bottom,
                });
            }
        }
        rows.push(Row {
            lane,
            merge: parents.len() > 1,
            segments,
        });
    }
    rows
}

/// How many lanes a laid-out history uses, which is how wide the rail has
/// to be.
pub fn width(rows: &[Row]) -> usize {
    rows.iter()
        .flat_map(Row::lanes)
        .max()
        .map(|last| last + 1)
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(pairs: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        pairs
            .iter()
            .map(|(sha, parents)| {
                (
                    sha.to_string(),
                    parents.iter().map(|p| p.to_string()).collect(),
                )
            })
            .collect()
    }

    fn lay(pairs: &[(&str, &[&str])]) -> Vec<Row> {
        let owned = history(pairs);
        lay_out(
            owned
                .iter()
                .map(|(sha, parents)| (sha.as_str(), parents.as_slice())),
        )
    }

    fn halves(row: &Row, half: Half) -> Vec<(usize, usize)> {
        let mut found: Vec<(usize, usize)> = row
            .segments
            .iter()
            .filter(|segment| segment.half == half)
            .map(|segment| (segment.from, segment.to))
            .collect();
        found.sort_unstable();
        found
    }

    #[test]
    fn a_straight_history_stays_in_one_lane() {
        let rows = lay(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]);
        assert_eq!(
            rows.iter().map(|row| row.lane).collect::<Vec<_>>(),
            [0, 0, 0]
        );
        assert!(rows.iter().all(|row| !row.merge));
        // The tip has nothing above it and the root nothing below.
        assert_eq!(halves(&rows[0], Half::Top), []);
        assert_eq!(halves(&rows[0], Half::Bottom), [(0, 0)]);
        assert_eq!(halves(&rows[1], Half::Top), [(0, 0)]);
        assert_eq!(halves(&rows[2], Half::Bottom), []);
        assert_eq!(width(&rows), 1);
    }

    #[test]
    fn a_merge_sends_a_thread_out_and_the_meeting_brings_it_back() {
        // m merges the side branch s back into the trunk t.
        let rows = lay(&[
            ("m", &["t", "s"]),
            ("t", &["base"]),
            ("s", &["base"]),
            ("base", &[]),
        ]);
        assert!(rows[0].merge);
        assert_eq!(rows[0].lane, 0);
        // Out of the merge: one thread carries on, one leaves for lane 1.
        assert_eq!(halves(&rows[0], Half::Bottom), [(0, 0), (0, 1)]);
        // The trunk keeps lane 0 and the side branch runs past it.
        assert_eq!(rows[1].lane, 0);
        assert_eq!(halves(&rows[1], Half::Top), [(0, 0), (1, 1)]);
        assert_eq!(
            rows[2].lane, 1,
            "the side branch is in the lane it left for"
        );
        // Both wait for the base, and at the base the second lane bends in.
        assert_eq!(rows[3].lane, 0);
        assert_eq!(halves(&rows[3], Half::Top), [(0, 0), (1, 0)]);
        assert_eq!(halves(&rows[3], Half::Bottom), []);
        assert_eq!(width(&rows), 2);
    }

    #[test]
    fn joining_a_lane_that_is_still_running_leaves_its_line_alone() {
        // A merges s into t; B, further down the trunk, merges the same s
        // again. The second bend joins a lane that has its own thread, and
        // cutting that thread is what left lines ending in mid-air.
        let rows = lay(&[
            ("a", &["b", "s"]),
            ("b", &["d", "s"]),
            ("d", &[]),
            ("s", &[]),
        ]);
        assert_eq!(halves(&rows[0], Half::Bottom), [(0, 0), (0, 1)]);
        assert_eq!(
            halves(&rows[1], Half::Bottom),
            [(0, 0), (0, 1), (1, 1)],
            "the branch it joins keeps running below the join"
        );
    }

    #[test]
    fn a_lane_is_reused_once_its_thread_has_ended() {
        // Two independent tips, the first of which ends immediately.
        let rows = lay(&[("one", &[]), ("two", &["three"]), ("three", &[])]);
        assert_eq!(rows[0].lane, 0);
        assert_eq!(rows[1].lane, 0, "nothing was left running to keep lane 0");
        assert_eq!(width(&rows), 1);
    }

    #[test]
    fn nothing_lays_out_as_nothing() {
        let rows = lay(&[]);
        assert!(rows.is_empty());
        assert_eq!(width(&rows), 1, "an empty rail is still one lane wide");
    }
}
