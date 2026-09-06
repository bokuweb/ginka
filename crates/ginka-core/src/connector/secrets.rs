//! Where a connector's tokens come from, and where they never go.
//!
//! Tokens live in `~/.ginka/connectors/<connector>.env` with mode `0600`, or
//! in the environment, the environment winning. They are never written to
//! `settings.json`, never to SQLite, and never to a log line
//! (`docs/connectors.md` §4.1).

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// The two tokens the Slack connector needs.
#[derive(Clone, PartialEq, Eq)]
pub struct SlackTokens {
    /// `xoxb-…`, for the Web API.
    pub bot: String,
    /// `xapp-…`, for Socket Mode.
    pub app: String,
}

impl std::fmt::Debug for SlackTokens {
    /// Never the values: a `{:?}` in a log line is exactly how a token leaks.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackTokens")
            .field("bot", &redact(&self.bot))
            .field("app", &redact(&self.app))
            .finish()
    }
}

/// The variable a bot token is read from.
pub const BOT_TOKEN_VAR: &str = "GINKA_SLACK_BOT_TOKEN";
/// The variable an app-level token is read from.
pub const APP_TOKEN_VAR: &str = "GINKA_SLACK_APP_TOKEN";

/// What a file or the environment said, before it is judged complete.
#[derive(Debug, Default)]
struct Found {
    bot: Option<String>,
    app: Option<String>,
}

/// Read the Slack tokens from the environment, then from `file`.
///
/// `Ok(None)` is the ordinary "not configured" answer: no file and no
/// variables. A file that exists but is missing one of the two is an error,
/// because half a configuration is a mistake to report, not a state to run
/// in.
pub fn slack_tokens(file: &Path) -> Result<Option<SlackTokens>> {
    let mut found = Found {
        bot: std::env::var(BOT_TOKEN_VAR).ok().filter(|v| !v.is_empty()),
        app: std::env::var(APP_TOKEN_VAR).ok().filter(|v| !v.is_empty()),
    };
    let file_exists = file.exists();
    if file_exists {
        let text =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let values = parse_env(&text);
        found.bot = found.bot.or_else(|| values.get(BOT_TOKEN_VAR).cloned());
        found.app = found.app.or_else(|| values.get(APP_TOKEN_VAR).cloned());
    }
    match (found.bot, found.app) {
        (Some(bot), Some(app)) => Ok(Some(SlackTokens { bot, app })),
        (None, None) => Ok(None),
        (bot, _) => {
            let missing = if bot.is_none() {
                BOT_TOKEN_VAR
            } else {
                APP_TOKEN_VAR
            };
            anyhow::bail!(
                "{missing} is not set; both tokens are needed, in the environment or in {}",
                file.display()
            )
        }
    }
}

/// Whether a secrets file is readable by anyone but its owner.
///
/// Reported by `doctor` and `slack status`; never fixed silently, because
/// a file the user made readable on purpose is theirs to explain.
#[cfg(unix)]
pub fn is_too_open(file: &Path) -> Option<bool> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(file).ok()?.permissions().mode();
    Some(mode & 0o077 != 0)
}

#[cfg(not(unix))]
pub fn is_too_open(_file: &Path) -> Option<bool> {
    None
}

/// `KEY=VALUE` lines, `#` comments, optional `export`, optional quotes.
fn parse_env(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        if !value.is_empty() {
            values.insert(key.trim().to_string(), value.to_string());
        }
    }
    values
}

/// The first few characters and nothing else, for a log line.
pub fn redact(token: &str) -> String {
    let shown: String = token.chars().take(5).collect();
    format!("{shown}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_env_file_is_read_with_its_comments_quotes_and_exports() {
        let values = parse_env(
            "# tokens\nexport GINKA_SLACK_BOT_TOKEN=\"xoxb-1\"\nGINKA_SLACK_APP_TOKEN='xapp-2'\n\nJUNK\n",
        );
        assert_eq!(
            values.get(BOT_TOKEN_VAR).map(String::as_str),
            Some("xoxb-1")
        );
        assert_eq!(
            values.get(APP_TOKEN_VAR).map(String::as_str),
            Some("xapp-2")
        );
        assert!(!values.contains_key("JUNK"));
    }

    #[test]
    fn no_file_and_no_variables_means_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        // The variables may be set in the shell running the tests; only the
        // file's absence is under our control here.
        if std::env::var(BOT_TOKEN_VAR).is_ok() || std::env::var(APP_TOKEN_VAR).is_ok() {
            return;
        }
        assert!(
            slack_tokens(&dir.path().join("slack.env"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn half_a_configuration_is_an_error_not_a_state() {
        if std::env::var(BOT_TOKEN_VAR).is_ok() || std::env::var(APP_TOKEN_VAR).is_ok() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("slack.env");
        std::fs::write(&file, "GINKA_SLACK_BOT_TOKEN=xoxb-1\n").unwrap();
        let error = slack_tokens(&file).unwrap_err().to_string();
        assert!(error.contains(APP_TOKEN_VAR), "{error}");
    }

    #[test]
    fn a_token_never_prints_whole() {
        let tokens = SlackTokens {
            bot: "xoxb-secret-secret".into(),
            app: "xapp-secret-secret".into(),
        };
        let printed = format!("{tokens:?}");
        assert!(!printed.contains("secret"));
        assert!(printed.contains("xoxb-…"));
    }
}
