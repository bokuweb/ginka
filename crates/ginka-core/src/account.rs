//! Accounts: several logins per provider (`docs/accounts.md`).
//!
//! An account is a directory the vendor's CLI keeps one login in, and a label.
//! It is not a credential: Ginka creates the directory, points the CLI at it
//! through the variable the driver names, and never reads what the vendor
//! wrote inside. The default account of a provider is the vendor's own home,
//! contributes nothing to the environment, and is never in the settings file
//! — so one login per provider is exactly the setup that existed before this
//! module did.
//!
//! The record lives in `settings.json` rather than the database, for the same
//! reasons the per-agent settings do: a person edits it by hand, `doctor` has
//! to read it when the daemon will not start, and nothing in it is indexed.

use crate::driver::Registry;
use crate::paths::Paths;
use crate::settings::{AccountSettings, DaemonSettings};
use ginka_protocol::AccountId;
use ginka_protocol::ids::slugify;
use ginka_protocol::model::{Account, LoginCommand};
use ginka_protocol::provider::ProviderKind;
use std::path::PathBuf;

/// What can go wrong adding, removing or naming an account.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    /// The id is not a slug.
    #[error(
        "an account id is a slug — lowercase letters, digits and single dashes, like `claude-work`"
    )]
    BadId,
    /// The id is a provider's own, which names its default account.
    #[error("`{0}` is a provider's default account; it is always there and cannot be removed")]
    IsDefault(String),
    /// An account with this id already exists.
    #[error("an account named `{0}` already exists")]
    Exists(String),
    /// No account has this id.
    #[error("no account named `{0}`")]
    Unknown(String),
    /// The account belongs to a different provider than the one asked for.
    #[error("account `{account}` is a {actual} login, not a {expected} one")]
    WrongProvider {
        /// The account id that was asked for.
        account: String,
        /// The provider the caller wanted a login for.
        expected: String,
        /// The provider whose default account that id names.
        actual: ProviderKind,
    },
    /// The provider's CLI reads its state from one fixed place.
    #[error("{0} keeps one login per machine; its CLI cannot be pointed at a second directory")]
    OneLoginOnly(ProviderKind),
    /// This build has no driver for the provider.
    #[error("this build has no driver for {0}")]
    NoDriver(ProviderKind),
    /// The directory could not be created or removed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Where an account's directory is: `~/.ginka/accounts/<id>/`.
///
/// The value handed to the driver's home variable. A daemon-host path
/// (`docs/roadmap.md` §4.1).
pub fn home_dir(paths: &Paths, id: &AccountId) -> PathBuf {
    paths.accounts().join(&id.0)
}

/// Check that an id is a slug and not a provider's own.
///
/// The slug rule is the one every other id uses: an account id ends up in a
/// path and on a command line.
pub fn validate_id(id: &AccountId) -> Result<(), AccountError> {
    if id.0.is_empty() || slugify(&id.0) != id.0 {
        return Err(AccountError::BadId);
    }
    if id.is_default() {
        return Err(AccountError::IsDefault(id.0.clone()));
    }
    Ok(())
}

/// Which account a session runs on.
///
/// The one asked for, checked against the provider it is for — a Codex login
/// cannot run a Claude session — or the provider's active account when none
/// was requested. A missing or stale active choice falls back to the
/// provider's system default. `provider` is the driver's id, which is what a
/// session records.
pub fn resolve(
    settings: &DaemonSettings,
    provider: &str,
    requested: Option<&AccountId>,
) -> Result<AccountId, AccountError> {
    let Some(requested) = requested else {
        return Ok(settings
            .active_accounts
            .get(provider)
            .filter(|id| {
                id.0 == provider
                    || settings
                        .accounts
                        .get(&id.0)
                        .is_some_and(|account| account.provider.as_str() == provider)
            })
            .cloned()
            .unwrap_or_else(|| AccountId(provider.to_string())));
    };
    if requested.0 == provider {
        return Ok(requested.clone());
    }
    if requested.is_default() {
        // Another provider's default: a `codex` login asked to run `claude`.
        return Err(AccountError::WrongProvider {
            account: requested.0.clone(),
            expected: provider.to_string(),
            actual: ProviderKind::parse(&requested.0).expect("is_default checked it"),
        });
    }
    let account = settings
        .accounts
        .get(&requested.0)
        .ok_or_else(|| AccountError::Unknown(requested.0.clone()))?;
    if account.provider.as_str() != provider {
        return Err(AccountError::WrongProvider {
            account: requested.0.clone(),
            expected: provider.to_string(),
            actual: account.provider,
        });
    }
    Ok(requested.clone())
}

/// Select the account future sessions of its provider use.
///
/// Existing sessions are unaffected because their account id is stored on the
/// session. Selecting the provider's own default removes the preference so a
/// hand-edited settings file remains sparse.
pub fn select(settings: &mut DaemonSettings, id: &AccountId) -> Result<(), AccountError> {
    let provider = provider_of(settings, id)?;
    if id.0 == provider {
        settings.active_accounts.remove(&provider);
    } else {
        settings.active_accounts.insert(provider, id.clone());
    }
    Ok(())
}

/// The account's layer of an agent's environment (`docs/accounts.md` §4).
///
/// Applied after the provider's settings and before the session's own, so a
/// home variable typed into the provider's `env` by hand cannot defeat the
/// account a chat was aimed at, and a test can still override anything. The
/// default account contributes nothing, which is what "one login changes
/// nothing" means mechanically.
pub fn env_layer(
    settings: &DaemonSettings,
    paths: &Paths,
    id: &AccountId,
    home_variable: Option<&str>,
) -> Vec<(String, String)> {
    if id.is_default() {
        return Vec::new();
    }
    let mut layer = Vec::new();
    if let Some(variable) = home_variable {
        layer.push((
            variable.to_string(),
            home_dir(paths, id).to_string_lossy().into_owned(),
        ));
    }
    if let Some(account) = settings.accounts.get(&id.0) {
        layer.extend(
            account
                .env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    layer
}

/// Add an account: a directory for the provider's CLI to sign into.
///
/// The directory is created empty and private (`0700`): what the vendor puts
/// in it is a login. The settings are changed in memory; saving them is the
/// caller's, so a directory is never created for a record that was not kept.
pub fn add(
    settings: &mut DaemonSettings,
    paths: &Paths,
    id: AccountId,
    provider: ProviderKind,
    label: String,
    home_variable: Option<&str>,
) -> Result<(), AccountError> {
    validate_id(&id)?;
    if home_variable.is_none() {
        return Err(AccountError::OneLoginOnly(provider));
    }
    if settings.accounts.contains_key(&id.0) {
        return Err(AccountError::Exists(id.0));
    }
    let home = home_dir(paths, &id);
    std::fs::create_dir_all(&home)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))?;
    }
    let label = label.trim();
    settings.accounts.insert(
        id.0.clone(),
        AccountSettings {
            provider,
            label: if label.is_empty() {
                id.0.clone()
            } else {
                label.to_string()
            },
            env: Default::default(),
        },
    );
    Ok(())
}

/// Forget an account.
///
/// The directory holds the vendor's login, which is the thing a person least
/// wants deleted by accident, so it stays unless `delete_home` says otherwise.
pub fn remove(
    settings: &mut DaemonSettings,
    paths: &Paths,
    id: &AccountId,
    delete_home: bool,
) -> Result<(), AccountError> {
    if id.is_default() {
        return Err(AccountError::IsDefault(id.0.clone()));
    }
    let provider = settings
        .accounts
        .get(&id.0)
        .map(|account| account.provider.as_str().to_string())
        .ok_or_else(|| AccountError::Unknown(id.0.clone()))?;
    settings.accounts.remove(&id.0);
    if settings.active_accounts.get(&provider) == Some(id) {
        settings.active_accounts.remove(&provider);
    }
    if delete_home {
        let home = home_dir(paths, id);
        if home.exists() {
            std::fs::remove_dir_all(home)?;
        }
    }
    Ok(())
}

/// The provider an account belongs to, as a driver id.
///
/// A default account's provider is its own id; anything else is looked up.
pub fn provider_of(settings: &DaemonSettings, id: &AccountId) -> Result<String, AccountError> {
    if id.is_default() {
        return Ok(id.0.clone());
    }
    settings
        .accounts
        .get(&id.0)
        .map(|account| account.provider.as_str().to_string())
        .ok_or_else(|| AccountError::Unknown(id.0.clone()))
}

/// Every account as a client sees it, without the probe: the defaults first,
/// in the order the drivers are offered, each followed by its own provider's
/// logins.
///
/// `signed_in` is left `None`; asking the vendor is the service's job, and
/// its answer is cached there. An account whose provider this build has no
/// driver for is still listed — a settings file outlives the build that reads
/// it — but with no way to sign into it.
pub fn list(settings: &DaemonSettings, paths: &Paths, drivers: &Registry) -> Vec<Account> {
    let mut accounts = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for driver_id in drivers.ids() {
        let Some(driver) = drivers.get(driver_id) else {
            continue;
        };
        let Some(provider) = ProviderKind::parse(driver_id) else {
            continue;
        };
        let active =
            resolve(settings, driver_id, None).unwrap_or_else(|_| AccountId(driver_id.to_string()));
        let login = |env: Vec<(String, String)>| {
            driver.login_command().map(|command| LoginCommand {
                program: command.program,
                args: command.args,
                env,
            })
        };
        accounts.push(Account {
            id: AccountId::default_for(provider),
            provider,
            label: driver.display_name().to_string(),
            home: None,
            is_default: true,
            active: active.0 == driver_id,
            env_keys: Vec::new(),
            signed_in: None,
            login: login(Vec::new()),
            identity: None,
        });
        for (id, account) in &settings.accounts {
            if account.provider != provider {
                continue;
            }
            let id = AccountId(id.clone());
            let home = home_dir(paths, &id);
            let env = driver
                .home_variable()
                .map(|variable| vec![(variable.to_string(), home.to_string_lossy().into_owned())])
                .unwrap_or_default();
            seen.insert(id.0.clone());
            accounts.push(Account {
                active: active == id,
                id,
                provider,
                label: account.label.clone(),
                home: Some(home),
                is_default: false,
                env_keys: account.env.keys().cloned().collect(),
                signed_in: None,
                login: login(env),
                identity: None,
            });
        }
    }

    for (id, account) in &settings.accounts {
        if seen.contains(id) {
            continue;
        }
        let id = AccountId(id.clone());
        accounts.push(Account {
            active: settings.active_accounts.get(account.provider.as_str()) == Some(&id),
            home: Some(home_dir(paths, &id)),
            id,
            provider: account.provider,
            label: account.label.clone(),
            is_default: false,
            env_keys: account.env.keys().cloned().collect(),
            signed_in: None,
            login: None,
            identity: None,
        });
    }
    accounts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, Paths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(tmp.path().join("state"));
        (tmp, paths)
    }

    #[test]
    fn an_id_is_a_slug_and_never_a_providers_own() {
        assert!(validate_id(&AccountId("claude-work".into())).is_ok());
        assert!(matches!(
            validate_id(&AccountId("Claude Work".into())),
            Err(AccountError::BadId)
        ));
        assert!(matches!(
            validate_id(&AccountId(String::new())),
            Err(AccountError::BadId)
        ));
        // `codex` names Codex's default account, which is always there.
        assert!(matches!(
            validate_id(&AccountId("codex".into())),
            Err(AccountError::IsDefault(_))
        ));
    }

    #[test]
    fn adding_makes_a_private_directory_and_a_record() {
        let (_tmp, paths) = paths();
        let mut settings = DaemonSettings::default();
        add(
            &mut settings,
            &paths,
            AccountId("codex-work".into()),
            ProviderKind::Codex,
            "  Work ".into(),
            Some("CODEX_HOME"),
        )
        .unwrap();
        let home = home_dir(&paths, &AccountId("codex-work".into()));
        assert!(home.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&home).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "a login directory is nobody else's");
        }
        let record = &settings.accounts["codex-work"];
        assert_eq!(record.provider, ProviderKind::Codex);
        assert_eq!(record.label, "Work", "trimmed");
        assert!(record.env.is_empty());
    }

    #[test]
    fn a_provider_with_no_home_variable_cannot_have_a_second_login() {
        let (_tmp, paths) = paths();
        let mut settings = DaemonSettings::default();
        let refused = add(
            &mut settings,
            &paths,
            AccountId("amp-two".into()),
            ProviderKind::Amp,
            "Two".into(),
            None,
        );
        assert!(matches!(refused, Err(AccountError::OneLoginOnly(_))));
        assert!(settings.accounts.is_empty());
        assert!(!home_dir(&paths, &AccountId("amp-two".into())).exists());
    }

    #[test]
    fn adding_the_same_id_twice_is_refused() {
        let (_tmp, paths) = paths();
        let mut settings = DaemonSettings::default();
        let id = AccountId("claude-work".into());
        add(
            &mut settings,
            &paths,
            id.clone(),
            ProviderKind::Claude,
            "Work".into(),
            Some("CLAUDE_CONFIG_DIR"),
        )
        .unwrap();
        assert!(matches!(
            add(
                &mut settings,
                &paths,
                id,
                ProviderKind::Claude,
                "Again".into(),
                Some("CLAUDE_CONFIG_DIR")
            ),
            Err(AccountError::Exists(_))
        ));
    }

    #[test]
    fn removing_keeps_the_login_unless_told_otherwise() {
        let (_tmp, paths) = paths();
        let mut settings = DaemonSettings::default();
        let id = AccountId("claude-work".into());
        add(
            &mut settings,
            &paths,
            id.clone(),
            ProviderKind::Claude,
            "Work".into(),
            Some("CLAUDE_CONFIG_DIR"),
        )
        .unwrap();
        let home = home_dir(&paths, &id);

        remove(&mut settings, &paths, &id, false).unwrap();
        assert!(settings.accounts.is_empty());
        assert!(home.is_dir(), "the vendor's login is not ours to delete");

        settings.accounts.insert(
            id.0.clone(),
            AccountSettings {
                provider: ProviderKind::Claude,
                label: "Work".into(),
                env: Default::default(),
            },
        );
        remove(&mut settings, &paths, &id, true).unwrap();
        assert!(!home.exists());

        assert!(matches!(
            remove(&mut settings, &paths, &id, false),
            Err(AccountError::Unknown(_))
        ));
        assert!(matches!(
            remove(&mut settings, &paths, &AccountId("claude".into()), true),
            Err(AccountError::IsDefault(_))
        ));
    }

    #[test]
    fn the_default_account_adds_nothing_to_the_environment() {
        let (_tmp, paths) = paths();
        let settings = DaemonSettings::default();
        assert!(
            env_layer(
                &settings,
                &paths,
                &AccountId("claude".into()),
                Some("CLAUDE_CONFIG_DIR")
            )
            .is_empty()
        );
    }

    #[test]
    fn a_named_account_points_the_cli_at_its_directory_then_adds_its_own_variables() {
        let (_tmp, paths) = paths();
        let mut settings = DaemonSettings::default();
        let id = AccountId("claude-work".into());
        add(
            &mut settings,
            &paths,
            id.clone(),
            ProviderKind::Claude,
            "Work".into(),
            Some("CLAUDE_CONFIG_DIR"),
        )
        .unwrap();
        settings
            .accounts
            .get_mut("claude-work")
            .unwrap()
            .env
            .insert("ANTHROPIC_BASE_URL".into(), "https://gateway".into());

        let layer = env_layer(&settings, &paths, &id, Some("CLAUDE_CONFIG_DIR"));
        assert_eq!(
            layer,
            vec![
                (
                    "CLAUDE_CONFIG_DIR".to_string(),
                    home_dir(&paths, &id).to_string_lossy().into_owned()
                ),
                (
                    "ANTHROPIC_BASE_URL".to_string(),
                    "https://gateway".to_string()
                ),
            ]
        );
    }

    #[test]
    fn a_session_runs_on_the_account_asked_for_or_the_providers_default() {
        let (_tmp, paths) = paths();
        let mut settings = DaemonSettings::default();
        add(
            &mut settings,
            &paths,
            AccountId("codex-work".into()),
            ProviderKind::Codex,
            "Work".into(),
            Some("CODEX_HOME"),
        )
        .unwrap();

        assert_eq!(
            resolve(&settings, "claude", None).unwrap(),
            AccountId("claude".into())
        );
        assert_eq!(
            resolve(&settings, "codex", Some(&AccountId("codex-work".into()))).unwrap(),
            AccountId("codex-work".into())
        );
        assert_eq!(
            resolve(&settings, "codex", Some(&AccountId("codex".into()))).unwrap(),
            AccountId("codex".into())
        );
        // A Codex login cannot run a Claude session.
        assert!(matches!(
            resolve(&settings, "claude", Some(&AccountId("codex-work".into()))),
            Err(AccountError::WrongProvider { .. })
        ));
        assert!(matches!(
            resolve(&settings, "claude", Some(&AccountId("codex".into()))),
            Err(AccountError::WrongProvider { .. })
        ));
        assert!(matches!(
            resolve(&settings, "claude", Some(&AccountId("nonesuch".into()))),
            Err(AccountError::Unknown(_))
        ));
    }

    #[test]
    fn a_provider_remembers_the_account_selected_for_new_sessions() {
        let (_tmp, paths) = paths();
        let mut settings = DaemonSettings::default();
        let work = AccountId("codex-work".into());
        add(
            &mut settings,
            &paths,
            work.clone(),
            ProviderKind::Codex,
            "Work".into(),
            Some("CODEX_HOME"),
        )
        .unwrap();

        select(&mut settings, &work).unwrap();
        assert_eq!(resolve(&settings, "codex", None).unwrap(), work);

        select(&mut settings, &AccountId("codex".into())).unwrap();
        assert_eq!(
            resolve(&settings, "codex", None).unwrap(),
            AccountId("codex".into())
        );
    }

    #[test]
    fn removing_the_active_account_falls_back_to_the_system_default() {
        let (_tmp, paths) = paths();
        let mut settings = DaemonSettings::default();
        let work = AccountId("claude-work".into());
        add(
            &mut settings,
            &paths,
            work.clone(),
            ProviderKind::Claude,
            "Work".into(),
            Some("CLAUDE_CONFIG_DIR"),
        )
        .unwrap();
        select(&mut settings, &work).unwrap();

        remove(&mut settings, &paths, &work, false).unwrap();

        assert_eq!(
            resolve(&settings, "claude", None).unwrap(),
            AccountId("claude".into())
        );
    }

    #[test]
    fn the_list_puts_each_providers_default_before_its_logins() {
        let (_tmp, paths) = paths();
        let mut settings = DaemonSettings::default();
        for (id, provider) in [
            ("codex-work", ProviderKind::Codex),
            ("claude-work", ProviderKind::Claude),
        ] {
            add(
                &mut settings,
                &paths,
                AccountId(id.into()),
                provider,
                id.into(),
                Some("X"),
            )
            .unwrap();
        }
        settings
            .accounts
            .get_mut("claude-work")
            .unwrap()
            .env
            .insert("ANTHROPIC_API_KEY".into(), "sk-secret".into());

        let accounts = list(&settings, &paths, &Registry::with_defaults());
        let ids: Vec<&str> = accounts
            .iter()
            .map(|account| account.id.0.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "claude",
                "claude-work",
                "codex",
                "codex-work",
                "gemini",
                "opencode"
            ]
        );

        let default = &accounts[0];
        assert!(default.is_default);
        assert_eq!(default.home, None);
        assert!(default.login.as_ref().unwrap().env.is_empty());

        let work = &accounts[1];
        assert_eq!(work.home, Some(home_dir(&paths, &work.id)));
        // The name of the variable crosses the wire; the key never does.
        assert_eq!(work.env_keys, vec!["ANTHROPIC_API_KEY"]);
        assert!(!format!("{work:?}").contains("sk-secret"));
        let login = work.login.as_ref().unwrap();
        assert_eq!(login.env.len(), 1);
        assert_eq!(login.env[0].0, "CLAUDE_CONFIG_DIR");
    }

    #[test]
    fn an_account_for_a_provider_this_build_cannot_run_is_still_listed() {
        let (_tmp, paths) = paths();
        let mut settings = DaemonSettings::default();
        settings.accounts.insert(
            "gemini-work".into(),
            AccountSettings {
                provider: ProviderKind::Gemini,
                label: "Work".into(),
                env: Default::default(),
            },
        );
        let accounts = list(&settings, &paths, &Registry::with_defaults());
        let orphan = accounts
            .iter()
            .find(|account| account.id.0 == "gemini-work")
            .expect("a settings file outlives the build that reads it");
        assert!(orphan.login.is_none());
        assert_eq!(
            provider_of(&settings, &orphan.id).unwrap(),
            "gemini",
            "and it still says whose it is"
        );
    }
}
