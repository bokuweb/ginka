//! The Reports surface's view model: what the work cost, by day, by agent
//! and by login, and each login's rate-limit windows with the reading's age
//! (`docs/accounts.md` §11).
//!
//! Numbers are formatted here rather than in the view so the formatting has
//! tests: a cost of `None` prints as nothing, never as `$0.00`, because zero
//! would be a claim that the work was free.

use ginka_protocol::Usage;
use ginka_protocol::model::{PlanSnapshot, UsageRow, UsageTotals};

/// The usage report as the daemon answers it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageReport {
    pub by_day: Vec<UsageRow>,
    pub by_agent: Vec<UsageRow>,
    pub by_account: Vec<UsageRow>,
    /// The latest reading per login, for those that have one.
    pub plans: Vec<PlanSnapshot>,
}

impl UsageReport {
    /// Whether there is anything to draw at all.
    pub fn is_empty(&self) -> bool {
        self.by_day.is_empty() && self.plans.is_empty()
    }

    /// Replace one login's reading, or add it: what a push carries.
    pub fn set_plan(&mut self, snapshot: PlanSnapshot) {
        self.plans.retain(|known| known.account != snapshot.account);
        self.plans.push(snapshot);
    }
}

/// `12.3k in · 1.2k out · 4 turns · $0.42` — the totals of one row.
pub fn totals_line(totals: &UsageTotals) -> String {
    let mut parts = vec![
        format!("{} in", compact(totals.input_tokens)),
        format!("{} out", compact(totals.output_tokens)),
    ];
    if totals.cache_read_tokens > 0 {
        parts.push(format!("{} cached", compact(totals.cache_read_tokens)));
    }
    parts.push(format!("{} turns", totals.turns));
    if let Some(cost) = totals.cost_usd {
        parts.push(format!("${cost:.2}"));
    }
    parts.join(" · ")
}

/// One line per window of a reading: `5h · 92% · resets in 40m`, with *at
/// the wall* passed in by the caller, who owns the words.
pub fn window_lines(snapshot: &PlanSnapshot, now: i64, at_the_wall: &str) -> Vec<String> {
    snapshot
        .usage
        .windows
        .iter()
        .map(|window| {
            let mut parts = vec![window.label.clone(), format!("{:.0}%", window.used_percent)];
            if window.is_exhausted() {
                parts.push(at_the_wall.to_string());
            }
            let reset = window.reset_label(now);
            if !reset.is_empty() {
                parts.push(reset);
            }
            parts.join(" · ")
        })
        .collect()
}

/// A token count short enough for a row: `950`, `12.3k`, `4.0M`.
pub fn compact(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{:.1}k", count as f64 / 1_000.0),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

/// The current session's billed-token count in one chip-sized number.
///
/// Provider usage treats cache reads and reasoning as breakdowns of input and
/// output respectively, so adding those fields again would visibly overstate
/// consumption. `None` distinguishes no reading yet from a real zero.
pub fn session_token_count(usage: &Usage) -> Option<String> {
    let total = usage.input_tokens.saturating_add(usage.output_tokens);
    (total > 0).then(|| compact(total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ginka_protocol::AccountId;
    use ginka_protocol::model::{PlanSource, PlanUsage, PlanWindow};

    #[test]
    fn a_row_says_what_was_spent_and_prices_it_only_when_priced() {
        let priced = UsageTotals {
            input_tokens: 12_345,
            output_tokens: 1_200,
            cache_read_tokens: 0,
            reasoning_tokens: 0,
            cost_usd: Some(0.42),
            turns: 4,
        };
        assert_eq!(
            totals_line(&priced),
            "12.3k in · 1.2k out · 4 turns · $0.42"
        );
        let unpriced = UsageTotals {
            cost_usd: None,
            cache_read_tokens: 900,
            ..priced
        };
        // No `$0.00`: zero would be a claim that the work was free.
        assert_eq!(
            totals_line(&unpriced),
            "12.3k in · 1.2k out · 900 cached · 4 turns"
        );
    }

    #[test]
    fn counts_are_shortened_the_way_a_reader_rounds_them() {
        assert_eq!(compact(950), "950");
        assert_eq!(compact(12_345), "12.3k");
        assert_eq!(compact(4_000_000), "4.0M");
    }

    #[test]
    fn session_token_count_is_visible_without_double_counting_cache() {
        let usage = Usage {
            input_tokens: 12_345,
            output_tokens: 1_200,
            cache_read_tokens: 9_000,
            reasoning_tokens: 600,
            cost_usd: Some(0.42),
        };

        assert_eq!(session_token_count(&usage).as_deref(), Some("13.5k"));
        assert_eq!(session_token_count(&Usage::default()), None);
    }

    #[test]
    fn every_window_of_a_reading_gets_a_line() {
        let snapshot = PlanSnapshot {
            account: AccountId("codex".into()),
            usage: PlanUsage {
                plan: Some("pro".into()),
                windows: vec![
                    PlanWindow {
                        label: "5h".into(),
                        used_percent: 100.0,
                        resets_at: Some(1_000 + 2_400),
                    },
                    PlanWindow {
                        label: "week".into(),
                        used_percent: 40.0,
                        resets_at: None,
                    },
                ],
            },
            observed_at: 1_000,
            source: PlanSource::Fetched,
        };
        assert_eq!(
            window_lines(&snapshot, 1_000, "at the wall"),
            vec!["5h · 100% · at the wall · resets in 40m", "week · 40%"]
        );
    }

    #[test]
    fn a_pushed_reading_replaces_the_last_one_for_its_login() {
        let reading = |used: f64| PlanSnapshot {
            account: AccountId("codex".into()),
            usage: PlanUsage {
                plan: None,
                windows: vec![PlanWindow {
                    label: "week".into(),
                    used_percent: used,
                    resets_at: None,
                }],
            },
            observed_at: 0,
            source: PlanSource::Reported,
        };
        let mut report = UsageReport::default();
        assert!(report.is_empty());
        report.set_plan(reading(40.0));
        report.set_plan(reading(41.0));
        assert_eq!(report.plans.len(), 1);
        assert_eq!(report.plans[0].usage.windows[0].used_percent, 41.0);
        assert!(!report.is_empty());
    }
}
