//! The daemon's on/off settings as the settings page offers them.
//!
//! The daemon answers its settings as one JSON document (`DaemonSettings`)
//! and takes changes one key at a time (`UpdateDaemonSettings`), the same
//! surface `ginka settings` uses. The page shows only the switches a reader
//! is expected to flip; the rest stay a CLI concern.

/// The switches the settings page offers, in the order it lists them.
pub const TOGGLES: [&str; 4] = [
    "keep_awake",
    "resume_after_limit",
    "fetch_rates",
    "scan_vendor_logs",
];

/// Each offered switch the daemon's settings document has, with its value.
/// A key missing or not a boolean — an older or newer daemon — is left out
/// rather than shown wrong; a document that does not parse offers nothing.
pub fn toggles(json: &str) -> Vec<(&'static str, bool)> {
    let Ok(serde_json::Value::Object(fields)) = serde_json::from_str(json) else {
        return Vec::new();
    };
    TOGGLES
        .into_iter()
        .filter_map(|key| fields.get(key)?.as_bool().map(|on| (key, on)))
        .collect()
}

/// The `UpdateDaemonSettings` value that sets a switch.
pub fn value(on: bool) -> String {
    on.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offered_switches_are_read_in_order() {
        let json = r#"{
            "port": 0,
            "scan_vendor_logs": false,
            "keep_awake": true,
            "fetch_rates": true,
            "resume_after_limit": false
        }"#;
        assert_eq!(
            toggles(json),
            [
                ("keep_awake", true),
                ("resume_after_limit", false),
                ("fetch_rates", true),
                ("scan_vendor_logs", false),
            ]
        );
    }

    #[test]
    fn a_missing_or_mistyped_switch_is_left_out() {
        let json = r#"{"keep_awake": "yes", "fetch_rates": true}"#;
        assert_eq!(toggles(json), [("fetch_rates", true)]);
        assert!(toggles("not json").is_empty());
        assert!(toggles("[]").is_empty());
    }

    #[test]
    fn every_offered_switch_is_one_the_daemon_has() {
        // Pinned against the daemon's own type, so renaming a setting there
        // cannot quietly drop its switch from the page.
        let json = serde_json::to_string(&ginka_core::settings::DaemonSettings::default()).unwrap();
        assert_eq!(toggles(&json).len(), TOGGLES.len());
    }

    #[test]
    fn a_switch_is_written_as_json() {
        assert_eq!(value(true), "true");
        assert_eq!(value(false), "false");
    }
}
