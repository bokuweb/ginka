//! N12/N13: usage shows rate-limit headroom as well as totals, and costs come
//! from a rate table that says when it is not sure.

use ginka_core::usage::{
    self, CostQuality, ModelRate, PlanUsage, PlanWindow, RateTable, TokenTotals, UsageEvent,
};
use ginka_protocol::provider::ProviderKind;

/// 2026-09-04T00:00:00Z
const DAY_ONE: i64 = 1_788_480_000;
const DAY_TWO: i64 = DAY_ONE + 86_400;

fn event(model: &str, at: i64, input: u64, output: u64) -> UsageEvent {
    UsageEvent {
        session: "a".into(),
        provider: ProviderKind::Claude,
        model: model.into(),
        tokens: TokenTotals {
            input,
            output,
            ..TokenTotals::default()
        },
        at,
    }
}

fn rates() -> RateTable {
    let mut table = RateTable::empty(DAY_ONE);
    table.insert(
        "claude-haiku",
        ModelRate {
            input_per_million: 1.0,
            output_per_million: 5.0,
            cache_read_per_million: 0.1,
            cache_write_per_million: 1.25,
        },
    );
    table
}

#[test]
fn totals_count_every_kind_of_token() {
    let totals = TokenTotals {
        input: 10,
        output: 20,
        cache_read: 30,
        cache_write: 40,
    };
    assert_eq!(totals.total(), 100);
    assert_eq!(totals.billable_input(), 80, "cache tokens are still input");
}

#[test]
fn a_cost_is_priced_per_million_tokens() {
    let cost = rates()
        .cost_of(
            "claude-haiku",
            &TokenTotals {
                input: 1_000_000,
                output: 1_000_000,
                cache_read: 1_000_000,
                cache_write: 0,
            },
        )
        .unwrap();
    assert!((cost - 6.1).abs() < 1e-9, "{cost}");
}

#[test]
fn a_model_id_is_matched_without_its_vendor_prefix_or_date_suffix() {
    let table = rates();
    assert!(
        table
            .cost_of("anthropic/claude-haiku", &TokenTotals::default())
            .is_some()
    );
    assert!(
        table
            .cost_of("claude-haiku-20260101", &TokenTotals::default())
            .is_some()
    );
    assert!(
        table
            .cost_of("some-other-model", &TokenTotals::default())
            .is_none()
    );
}

#[test]
fn a_summary_groups_by_day_and_by_model() {
    let events = vec![
        event("claude-haiku", DAY_ONE, 100, 10),
        event("claude-haiku", DAY_ONE + 3_600, 200, 20),
        event("claude-opus", DAY_TWO, 300, 30),
    ];
    let summary = usage::summarize(&events, Some(&rates()));

    assert_eq!(summary.by_day.len(), 2);
    assert_eq!(summary.by_day[0].date, "2026-09-04");
    assert_eq!(summary.by_day[0].tokens.input, 300);
    assert_eq!(summary.by_day[1].date, "2026-09-05");

    assert_eq!(summary.by_model.len(), 2);
    assert_eq!(summary.by_model[0].model, "claude-haiku");
    assert_eq!(summary.by_model[0].tokens.total(), 330);
}

#[test]
fn an_unpriced_model_is_named_rather_than_costed_as_zero() {
    let events = vec![
        event("claude-haiku", DAY_ONE, 1_000_000, 0),
        event("claude-opus", DAY_ONE, 1_000_000, 0),
    ];
    let summary = usage::summarize(&events, Some(&rates()));

    assert_eq!(summary.cost, Some(1.0), "only the priced model is counted");
    assert_eq!(
        summary.quality,
        CostQuality::Partial {
            unpriced_models: vec!["claude-opus".into()]
        }
    );
}

#[test]
fn with_no_rate_table_the_tokens_still_report_and_the_cost_does_not_pretend() {
    let summary = usage::summarize(&[event("claude-haiku", DAY_ONE, 500, 50)], None);
    assert_eq!(summary.tokens.input, 500);
    assert_eq!(summary.cost, None);
    assert_eq!(summary.quality, CostQuality::Unpriced);
}

#[test]
fn a_fully_priced_summary_says_so() {
    let summary = usage::summarize(&[event("claude-haiku", DAY_ONE, 500, 50)], Some(&rates()));
    assert_eq!(summary.quality, CostQuality::Priced);
}

#[test]
fn an_empty_range_summarizes_to_nothing_without_dividing_by_zero() {
    let summary = usage::summarize(&[], Some(&rates()));
    assert_eq!(summary.tokens.total(), 0);
    assert_eq!(summary.cost, Some(0.0));
    assert!(summary.by_day.is_empty());
}

#[test]
fn a_rate_table_round_trips_through_its_cache_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("rates.json");
    rates().save(&path).unwrap();

    let loaded = RateTable::load(&path).unwrap().unwrap();
    assert_eq!(loaded, rates());
    assert!(
        RateTable::load(&tmp.path().join("absent.json"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_corrupt_cache_file_is_a_miss_rather_than_a_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("rates.json");
    std::fs::write(&path, "{ not json").unwrap();
    assert!(RateTable::load(&path).unwrap().is_none());
}

#[test]
fn a_table_older_than_its_lifetime_is_still_usable_but_known_to_be_stale() {
    let table = rates();
    assert!(!table.is_stale(DAY_ONE + 3_600));
    assert!(table.is_stale(DAY_TWO + 3_600));
    // Stale still prices: an offline user gets yesterday's rates, not nothing.
    assert!(
        table
            .cost_of("claude-haiku", &TokenTotals::default())
            .is_some()
    );
}

#[test]
fn a_plan_window_reports_headroom_and_when_it_comes_back() {
    let window = PlanWindow {
        label: "5-hour window".into(),
        used_percent: 62.5,
        resets_at: Some(DAY_ONE + 8_100),
    };
    assert_eq!(window.remaining_percent(), 37.5);
    assert_eq!(window.reset_label(DAY_ONE), "resets in 2h 15m");
    assert_eq!(window.reset_label(DAY_ONE + 8_100), "resets now");
    assert_eq!(window.reset_label(DAY_ONE + 9_000), "resets now");
}

#[test]
fn a_window_with_no_reset_time_says_nothing_rather_than_guessing() {
    let window = PlanWindow {
        label: "weekly".into(),
        used_percent: 10.0,
        resets_at: None,
    };
    assert_eq!(window.reset_label(DAY_ONE), "");
}

#[test]
fn a_percentage_outside_the_range_is_clamped() {
    let over = PlanWindow {
        label: "5-hour window".into(),
        used_percent: 140.0,
        resets_at: None,
    };
    assert_eq!(over.remaining_percent(), 0.0);
    assert!(over.is_exhausted());
}

#[test]
fn the_tightest_window_is_the_one_to_show() {
    let usage = PlanUsage {
        plan: Some("Pro".into()),
        windows: vec![
            PlanWindow {
                label: "weekly".into(),
                used_percent: 20.0,
                resets_at: None,
            },
            PlanWindow {
                label: "5-hour window".into(),
                used_percent: 91.0,
                resets_at: None,
            },
        ],
    };
    assert_eq!(usage.tightest().unwrap().label, "5-hour window");
    assert!(PlanUsage::default().tightest().is_none());
}
