//! A GitHub Actions job log, line by line, ready to draw.
//!
//! The runner writes every line with an ISO timestamp in front and marks
//! structure with `##[group]`, `##[error]` and their kin. A reader wants
//! the clock as a short time in the gutter and the markers as colour and
//! weight, not as text, so the log is walked once here. The runner does
//! not say which step a line belongs to; the job's steps say when each
//! ran, and [`assign`] puts the lines under them by the clock.

use chrono::{DateTime, Utc};

/// What a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Output.
    Plain,
    /// `##[group]`: the start of a foldable section, drawn as a heading.
    Group,
    /// `##[endgroup]`: the end of one; nothing to draw.
    EndGroup,
    /// `##[command]` or `##[section]`: what the runner ran.
    Command,
    /// `##[error]`.
    Error,
    /// `##[warning]`.
    Warning,
    /// `##[notice]` or `##[debug]`.
    Notice,
}

/// One line of a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// `HH:MM:SS`, when the line had a timestamp.
    pub time: Option<String>,
    /// The whole timestamp, for placing the line in a step.
    pub stamp: Option<DateTime<Utc>>,
    /// What it is.
    pub kind: Kind,
    /// The text, without the timestamp and without the marker.
    pub text: String,
}

/// Walk a log into lines. `##[endgroup]` lines are dropped: they say
/// where a group stops, and the next heading says that too.
pub fn parse(text: &str) -> Vec<Line> {
    text.lines()
        .map(|raw| raw.trim_start_matches('\u{feff}'))
        .filter_map(|raw| {
            let (stamp, rest) = split_time(raw);
            let (kind, text) = split_marker(rest);
            (kind != Kind::EndGroup).then(|| Line {
                time: stamp.map(|stamp| stamp.format("%H:%M:%S").to_string()),
                stamp,
                kind,
                text: text.to_string(),
            })
        })
        .collect()
}

/// `2026-09-07T01:58:00.7200450Z rest` → (the instant, `rest`).
fn split_time(raw: &str) -> (Option<DateTime<Utc>>, &str) {
    let Some((stamp, rest)) = raw.split_once(' ') else {
        return (None, raw);
    };
    let looks_like_time = stamp.len() >= 20
        && stamp.as_bytes().get(4) == Some(&b'-')
        && stamp.as_bytes().get(10) == Some(&b'T')
        && stamp.ends_with('Z');
    if !looks_like_time {
        return (None, raw);
    }
    match DateTime::parse_from_rfc3339(stamp) {
        Ok(at) => (Some(at.with_timezone(&Utc)), rest),
        Err(_) => (None, raw),
    }
}

/// Which step each line belongs to, by the clock.
///
/// `starts` are the steps' start times in order; a step that never ran
/// has none. A line goes with the last step that had started when it was
/// written. Lines before the first step, and lines with no clock, go with
/// the step before them — the first, at the top.
pub fn assign(lines: &[Line], starts: &[Option<DateTime<Utc>>]) -> Vec<usize> {
    let mut current = 0;
    lines
        .iter()
        .map(|line| {
            if let Some(stamp) = line.stamp {
                for (index, start) in starts.iter().enumerate().skip(current + 1) {
                    match start {
                        Some(start) if *start <= stamp => current = index,
                        Some(_) => break,
                        None => {}
                    }
                }
            }
            current
        })
        .collect()
}

fn split_marker(text: &str) -> (Kind, &str) {
    let Some(rest) = text.strip_prefix("##[") else {
        return (Kind::Plain, text);
    };
    let Some((marker, after)) = rest.split_once(']') else {
        return (Kind::Plain, text);
    };
    let kind = match marker {
        "group" => Kind::Group,
        "endgroup" => Kind::EndGroup,
        "command" | "section" => Kind::Command,
        "error" => Kind::Error,
        "warning" => Kind::Warning,
        "notice" | "debug" => Kind::Notice,
        _ => return (Kind::Plain, text),
    };
    (kind, after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runner_line_loses_its_stamp_and_keeps_its_clock() {
        let lines = parse("\u{feff}2026-09-07T01:58:00.7200450Z Current runner version: '2.337.0'");
        assert_eq!(lines[0].time.as_deref(), Some("01:58:00"));
        assert_eq!(lines[0].text, "Current runner version: '2.337.0'");
        assert_eq!(lines[0].kind, Kind::Plain);
    }

    #[test]
    fn markers_become_kinds_and_endgroup_is_dropped() {
        let text = "2026-09-07T01:58:00.7Z ##[group]Run cargo test\n\
                    2026-09-07T01:58:01.0Z ##[command]cargo test\n\
                    2026-09-07T01:58:02.0Z test result: ok\n\
                    2026-09-07T01:58:03.0Z ##[endgroup]\n\
                    2026-09-07T01:58:04.0Z ##[error]Process completed with exit code 1.\n\
                    2026-09-07T01:58:05.0Z ##[warning]deprecated";
        let lines = parse(text);
        let kinds: Vec<Kind> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Group,
                Kind::Command,
                Kind::Plain,
                Kind::Error,
                Kind::Warning
            ]
        );
        assert_eq!(lines[0].text, "Run cargo test");
        assert_eq!(lines[3].text, "Process completed with exit code 1.");
    }

    #[test]
    fn lines_fall_under_the_step_that_was_running() {
        let text = "2026-09-07T00:00:00.5Z a\n2026-09-07T00:00:03.2Z b\nno clock\n2026-09-07T00:00:08.0Z c";
        let lines = parse(text);
        let at = |s: &str| Some(DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc));
        let starts = [
            at("2026-09-07T00:00:00Z"),
            at("2026-09-07T00:00:03Z"),
            at("2026-09-07T00:00:08Z"),
        ];
        assert_eq!(assign(&lines, &starts), [0, 1, 1, 2]);
        // A step that never ran is passed over, not stopped at.
        let skipped = [starts[0], None, starts[2]];
        assert_eq!(assign(&lines, &skipped), [0, 0, 0, 2]);
        assert_eq!(assign(&lines, &[starts[0], None]), [0, 0, 0, 0]);
        assert_eq!(assign(&lines, &[]), [0, 0, 0, 0]);
    }

    #[test]
    fn a_line_without_a_stamp_is_left_alone() {
        let lines = parse("plain text here\nnot a 2026-09-07 stamp");
        assert_eq!(lines[0].time, None);
        assert_eq!(lines[0].text, "plain text here");
        assert_eq!(lines[1].text, "not a 2026-09-07 stamp");
    }
}
