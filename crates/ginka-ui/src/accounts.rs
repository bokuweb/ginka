//! Which login the composer starts on, and what its chip says
//! (`docs/accounts.md` §11).
//!
//! The chip exists only where there is a choice: a provider with one login
//! has nothing to pick, and a chip that could only restate the agent chip is
//! noise. What it says about headroom is the tightest window of the latest
//! reading, with the reading's age — a gauge without an age is a claim.

use ginka_protocol::AccountId;
use ginka_protocol::model::{Account, PlanSnapshot};

/// How old a reading may be before a chat about to start on the account is
/// worth a fresh one: the length of the shortest window vendors use.
pub const STALE_AFTER_SECS: i64 = 5 * 3_600;

/// The accounts of one provider, in the order the daemon lists them: the
/// default first, then its own logins.
pub fn accounts_for<'a>(accounts: &'a [Account], provider: &str) -> Vec<&'a Account> {
    accounts
        .iter()
        .filter(|account| account.provider.as_str() == provider)
        .collect()
}

/// Whether the chip is worth its space: only when there is something to
/// choose between.
pub fn offers_choice(accounts: &[Account], provider: &str) -> bool {
    accounts_for(accounts, provider).len() > 1
}

/// Which account the next prompt runs on.
///
/// The one the user chose, if it is this provider's — a choice made for the
/// last agent means nothing to the next — else the provider's persisted active
/// account, then its system default. `None` only when the daemon has said
/// nothing about the provider, in which case it runs the default anyway.
pub fn account_to_start<'a>(
    accounts: &'a [Account],
    provider: &str,
    chosen: Option<&AccountId>,
) -> Option<&'a Account> {
    let candidates = accounts_for(accounts, provider);
    chosen
        .and_then(|id| candidates.iter().find(|account| &account.id == id).copied())
        .or_else(|| candidates.iter().find(|account| account.active).copied())
        .or_else(|| {
            candidates
                .iter()
                .find(|account| account.is_default)
                .copied()
        })
        .or_else(|| candidates.first().copied())
}

/// The latest reading of one account's windows, if any.
pub fn snapshot_of<'a>(plans: &'a [PlanSnapshot], account: &AccountId) -> Option<&'a PlanSnapshot> {
    plans.iter().find(|snapshot| &snapshot.account == account)
}

/// What a chip says about an account's headroom: its tightest window.
#[derive(Debug, Clone, PartialEq)]
pub struct Headroom {
    /// The window's name: `5h`, `week`.
    pub window: String,
    pub used_percent: f64,
    /// At the wall. Said in words beside the number, never in colour alone.
    pub exhausted: bool,
    /// "resets in 40m", or empty when the vendor did not say.
    pub reset: String,
    /// How old the reading is, in seconds.
    pub age: i64,
}

impl Headroom {
    /// `5h 92%` — the two facts that fit on a chip.
    pub fn summary(&self) -> String {
        format!("{} {:.0}%", self.window, self.used_percent)
    }
}

/// The headroom to show for an account, from its latest reading.
///
/// `None` with no reading, or a reading with no windows in it: a gauge with
/// nothing on it is a claim, and the chip says nothing instead.
pub fn headroom(snapshot: Option<&PlanSnapshot>, now: i64) -> Option<Headroom> {
    let snapshot = snapshot?;
    let window = snapshot.usage.tightest()?;
    Some(Headroom {
        window: window.label.clone(),
        used_percent: window.used_percent,
        exhausted: window.is_exhausted(),
        reset: window.reset_label(now),
        age: (now - snapshot.observed_at).max(0),
    })
}

/// Whether an account's reading is worth refreshing before a chat starts on
/// it: there is none, or it is older than the shortest window.
pub fn wants_refresh(snapshot: Option<&PlanSnapshot>, now: i64) -> bool {
    match snapshot {
        None => true,
        Some(snapshot) => now - snapshot.observed_at > STALE_AFTER_SECS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ginka_protocol::model::{PlanSource, PlanUsage, PlanWindow};
    use ginka_protocol::provider::ProviderKind;

    fn account(id: &str, provider: ProviderKind, is_default: bool) -> Account {
        Account {
            id: AccountId(id.into()),
            provider,
            label: id.into(),
            home: None,
            is_default,
            active: is_default,
            env_keys: Vec::new(),
            signed_in: None,
            login: None,
            identity: None,
        }
    }

    fn accounts() -> Vec<Account> {
        vec![
            account("claude", ProviderKind::Claude, true),
            account("claude-work", ProviderKind::Claude, false),
            account("codex", ProviderKind::Codex, true),
        ]
    }

    fn snapshot(id: &str, used: f64, observed_at: i64) -> PlanSnapshot {
        PlanSnapshot {
            account: AccountId(id.into()),
            usage: PlanUsage {
                plan: Some("pro".into()),
                windows: vec![
                    PlanWindow {
                        label: "5h".into(),
                        used_percent: used,
                        resets_at: Some(observed_at + 2_400),
                    },
                    PlanWindow {
                        label: "week".into(),
                        used_percent: 12.0,
                        resets_at: None,
                    },
                ],
            },
            observed_at,
            source: PlanSource::Fetched,
        }
    }

    #[test]
    fn the_chip_exists_only_where_there_is_a_choice() {
        let accounts = accounts();
        assert!(offers_choice(&accounts, "claude"));
        assert!(
            !offers_choice(&accounts, "codex"),
            "one login: nothing to pick"
        );
        assert!(!offers_choice(&accounts, "gemini"));
    }

    #[test]
    fn a_choice_made_for_another_provider_means_nothing_to_this_one() {
        let accounts = accounts();
        let work = AccountId("claude-work".into());
        assert_eq!(
            account_to_start(&accounts, "claude", Some(&work)).map(|a| a.id.0.as_str()),
            Some("claude-work")
        );
        // The choice was Claude's; Codex runs its own default.
        assert_eq!(
            account_to_start(&accounts, "codex", Some(&work)).map(|a| a.id.0.as_str()),
            Some("codex")
        );
        assert_eq!(
            account_to_start(&accounts, "claude", None).map(|a| a.id.0.as_str()),
            Some("claude")
        );
        assert_eq!(account_to_start(&accounts, "gemini", None), None);
    }

    #[test]
    fn the_persisted_active_account_is_used_when_nothing_was_chosen_here() {
        let mut accounts = accounts();
        accounts[0].active = false;
        accounts[1].active = true;

        assert_eq!(
            account_to_start(&accounts, "claude", None).map(|account| account.id.0.as_str()),
            Some("claude-work")
        );
        assert_eq!(
            account_to_start(&accounts, "claude", Some(&AccountId("claude".into())))
                .map(|account| account.id.0.as_str()),
            Some("claude"),
            "an existing session keeps its recorded account"
        );
    }

    #[test]
    fn the_headroom_is_the_tightest_window_with_its_age() {
        let plans = vec![snapshot("claude-work", 92.0, 1_000)];
        let shown = headroom(snapshot_of(&plans, &AccountId("claude-work".into())), 1_300)
            .expect("there is a reading");
        assert_eq!(shown.summary(), "5h 92%");
        assert_eq!(shown.reset, "resets in 35m");
        assert_eq!(shown.age, 300);
        assert!(!shown.exhausted);
        // No reading is no claim.
        assert_eq!(
            headroom(snapshot_of(&plans, &AccountId("claude".into())), 1_300),
            None
        );
    }

    #[test]
    fn at_the_wall_is_a_word_beside_the_number() {
        let plans = [snapshot("codex", 100.0, 1_000)];
        let shown = headroom(plans.first(), 1_000).unwrap();
        assert!(shown.exhausted);
        assert_eq!(shown.summary(), "5h 100%");
    }

    #[test]
    fn a_reading_older_than_the_shortest_window_is_worth_refreshing() {
        let fresh = snapshot("codex", 10.0, 10_000);
        assert!(!wants_refresh(Some(&fresh), 10_000 + 3_600));
        assert!(wants_refresh(Some(&fresh), 10_000 + STALE_AFTER_SECS + 1));
        assert!(wants_refresh(None, 0), "no reading at all");
    }
}
