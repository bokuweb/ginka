//! Relative time, the way a list row says it.

use chrono::{DateTime, Datelike, Utc};

/// How long ago, in the shortest form that still says something: `now`,
/// `46m`, `4h`, `3d`, then the date.
///
/// After a month the count stops meaning anything and the date is shorter to
/// read; the year is added only when it is not this one.
pub fn age(now: DateTime<Utc>, then: DateTime<Utc>) -> String {
    let elapsed = now.signed_duration_since(then);
    let seconds = elapsed.num_seconds();
    if seconds < 60 {
        return rust_i18n::t!("time.now").to_string();
    }
    let minutes = elapsed.num_minutes();
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = elapsed.num_hours();
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = elapsed.num_days();
    if days < 30 {
        return format!("{days}d");
    }
    if then.year() == now.year() {
        then.format("%b %-d").to_string()
    } else {
        then.format("%b %-d, %Y").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn the_shortest_form_that_still_says_something() {
        let now = at("2026-09-05T12:00:00Z");
        assert_eq!(age(now, now - Duration::seconds(30)), "now");
        assert_eq!(age(now, now - Duration::minutes(46)), "46m");
        assert_eq!(age(now, now - Duration::hours(4)), "4h");
        assert_eq!(age(now, now - Duration::days(3)), "3d");
        assert_eq!(age(now, now - Duration::days(29)), "29d");
    }

    #[test]
    fn after_a_month_it_is_the_date_and_the_year_only_when_it_differs() {
        let now = Utc.with_ymd_and_hms(2026, 9, 5, 12, 0, 0).unwrap();
        assert_eq!(age(now, at("2026-03-02T00:00:00Z")), "Mar 2");
        assert_eq!(age(now, at("2025-12-24T00:00:00Z")), "Dec 24, 2025");
    }

    #[test]
    fn the_future_is_now() {
        let now = at("2026-09-05T12:00:00Z");
        assert_eq!(age(now, now + Duration::minutes(5)), "now");
    }
}
