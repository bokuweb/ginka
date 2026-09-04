//! N2/N3: which option changes a driver may absorb, and which always restart
//! the session no matter what the transport says it can do.

use ginka_protocol::provider::{
    AccessMode, OptionOutcome, ProviderKind, ProviderModel, ProviderOption, SessionOptions,
};

fn base() -> SessionOptions {
    SessionOptions {
        model: Some("sonnet".into()),
        reasoning_effort: Some("medium".into()),
        service_tier: None,
        access_mode: AccessMode::Ask,
    }
}

#[test]
fn an_unchanged_option_set_needs_nothing() {
    assert!(!SessionOptions::forces_restart(&base(), &base()));
    assert!(!base().differs_from(&base()));
}

#[test]
fn a_model_change_is_offered_to_the_driver() {
    let after = SessionOptions {
        model: Some("opus".into()),
        ..base()
    };
    assert!(after.differs_from(&base()));
    // Policy does not decide this one: the transport does.
    assert!(!SessionOptions::forces_restart(&base(), &after));
}

#[test]
fn a_reasoning_or_tier_change_is_also_the_drivers_call() {
    let effort = SessionOptions {
        reasoning_effort: Some("high".into()),
        ..base()
    };
    let tier = SessionOptions {
        service_tier: Some("fast".into()),
        ..base()
    };
    assert!(!SessionOptions::forces_restart(&base(), &effort));
    assert!(!SessionOptions::forces_restart(&base(), &tier));
}

#[test]
fn an_access_mode_change_always_restarts() {
    let after = SessionOptions {
        access_mode: AccessMode::Auto,
        ..base()
    };
    // Even a transport that carries the policy on every turn does not get to
    // widen what a running agent may touch.
    assert!(SessionOptions::forces_restart(&base(), &after));
}

#[test]
fn absorbed_and_restart_outcomes_are_distinguishable() {
    assert!(OptionOutcome::Absorbed.absorbed());
    assert!(!OptionOutcome::RestartRequired.absorbed());
}

#[test]
fn provider_kinds_round_trip_through_their_wire_names() {
    for kind in ProviderKind::ALL {
        assert_eq!(ProviderKind::parse(kind.as_str()), Some(kind));
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(serde_json::from_str::<ProviderKind>(&json).unwrap(), kind);
    }
    assert_eq!(ProviderKind::parse("nope"), None);
}

#[test]
fn a_catalogue_exposes_one_default_model() {
    let models = vec![
        ProviderModel::new("haiku", "Haiku"),
        ProviderModel::new("sonnet", "Sonnet").as_default(),
        ProviderModel::new("opus", "Opus"),
    ];
    assert_eq!(ProviderModel::default_of(&models).unwrap().id, "sonnet");
}

#[test]
fn a_catalogue_without_a_marked_default_falls_back_to_the_first() {
    let models = vec![
        ProviderModel::new("haiku", "Haiku"),
        ProviderModel::new("sonnet", "Sonnet"),
    ];
    assert_eq!(ProviderModel::default_of(&models).unwrap().id, "haiku");
    assert!(ProviderModel::default_of(&[]).is_none());
}

#[test]
fn a_model_can_carry_efforts_and_tiers() {
    let model = ProviderModel::new("opus", "Opus")
        .with_reasoning_efforts([
            ProviderOption::new("low", "Low"),
            ProviderOption::new("high", "High"),
        ])
        .with_service_tiers([ProviderOption::new("fast", "Fast")]);
    assert!(model.supports_reasoning_effort("high"));
    assert!(!model.supports_reasoning_effort("ultra"));
    assert!(model.supports_service_tier("fast"));
}
