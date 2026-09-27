//! Where the token comes from, and how one is obtained.
//!
//! The order of authority is the environment first — an explicit
//! `E1_GITHUB_TOKEN`, then the two names the ecosystem already uses — then
//! the keychain entry this app wrote when the reader signed in, and
//! `gh auth token` last, because it is the slowest and the one most people
//! already have. A token is never written to a plain file: signing in
//! stores it in the platform keychain ([`Keychain`]), where `gh` keeps its
//! own (`AGENTS.md` rule 8).
//!
//! Signing in is GitHub's device flow ([`device`]): the app shows a code,
//! the reader types it into a page in their browser, and the app polls
//! until GitHub hands it a token. No browser is embedded and no password
//! ever passes through this process.

use std::fmt;
use std::process::{Command, Stdio};

/// A bearer token. Its `Debug` output does not contain it.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// Wrap a token, trimmed. `None` for an empty string, so a variable set
    /// to nothing is the same as one not set.
    pub fn new(raw: impl AsRef<str>) -> Option<Self> {
        let trimmed = raw.as_ref().trim();
        (!trimmed.is_empty()).then(|| Self(trimmed.to_string()))
    }

    /// The token itself, for the `Authorization` header and nothing else.
    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(…)")
    }
}

/// The environment variables consulted, in order.
pub const ENVIRONMENT: [&str; 3] = ["E1_GITHUB_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"];

/// Where a token was found, which is what a sign-out has to undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// One of [`ENVIRONMENT`]. Signing out cannot remove it.
    Environment,
    /// This app's keychain entry. Signing out deletes it.
    Keychain,
    /// `gh auth token`. Signing out here does not sign `gh` out.
    Gh,
}

/// Pick a token from the environment, then the keychain, then `gh`.
///
/// Every source is injected so the order can be tested without process-wide
/// state: `env` answers a variable name, and the later sources are only
/// asked when the earlier ones had nothing.
pub fn resolve(
    env: impl Fn(&str) -> Option<String>,
    keychain: impl FnOnce() -> Option<String>,
    gh: impl FnOnce() -> Option<String>,
) -> Option<(Token, Source)> {
    ENVIRONMENT
        .iter()
        .find_map(|name| env(name).and_then(Token::new))
        .map(|token| (token, Source::Environment))
        .or_else(|| {
            keychain()
                .and_then(Token::new)
                .map(|token| (token, Source::Keychain))
        })
        .or_else(|| gh().and_then(Token::new).map(|token| (token, Source::Gh)))
}

/// What `gh auth token` prints, if `gh` is installed and signed in.
pub fn from_gh() -> Option<String> {
    let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The token this process should use, and where it came from.
pub fn discover() -> Option<(Token, Source)> {
    resolve(|name| std::env::var(name).ok(), Keychain::load, from_gh)
}

/// The OAuth app e1 signs in as unless told otherwise: registered under
/// `bokuweb`, with the device flow enabled and nothing else.
///
/// A client id is public by design — it names the app, it does not
/// authenticate it, and the device flow has no secret — so it lives in the
/// source rather than in a secret store.
pub const DEFAULT_CLIENT_ID: &str = "Ov23liJgXCd2OkbdPkM3";

/// The OAuth app this process signs in as.
///
/// `E1_GITHUB_CLIENT_ID` in the environment wins, then the value the binary
/// was built with, then [`DEFAULT_CLIENT_ID`]. A fork that registers its own
/// app sets the variable; everyone else signs in as e1.
pub fn client_id() -> String {
    std::env::var("E1_GITHUB_CLIENT_ID")
        .ok()
        .filter(|id| !id.trim().is_empty())
        .or_else(|| option_env!("E1_GITHUB_CLIENT_ID").map(str::to_string))
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string())
}

/// The platform keychain, through the `security` command on macOS.
///
/// Shelling out rather than linking Security.framework keeps the crate free
/// of a platform dependency it would only use here, and `security -i`
/// reads its commands from stdin so the token never appears in a process
/// listing. Elsewhere there is no keychain yet: every call answers as if
/// the entry did not exist.
pub struct Keychain;

/// The service name of the entry.
const SERVICE: &str = "e1.github.token";
/// The account name of the entry.
const ACCOUNT: &str = "e1";

impl Keychain {
    /// The stored token, if there is one.
    pub fn load() -> Option<String> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let output = Command::new("security")
            .args(["find-generic-password", "-s", SERVICE, "-a", ACCOUNT, "-w"])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Store a token, replacing any earlier one.
    pub fn store(token: &Token) -> std::io::Result<()> {
        if !cfg!(target_os = "macos") {
            return Err(std::io::Error::other("no keychain on this platform"));
        }
        // A GitHub token is `[A-Za-z0-9_]`; anything else is refused rather
        // than quoted, because a quoting bug here would store the wrong
        // secret silently.
        if !token
            .secret()
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(std::io::Error::other(
                "token has characters the keychain command cannot take",
            ));
        }
        use std::io::Write as _;
        let mut child = Command::new("security")
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            writeln!(
                stdin,
                "add-generic-password -U -s {SERVICE} -a {ACCOUNT} -w {}",
                token.secret()
            )?;
        }
        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "security exited with {status}"
            )))
        }
    }

    /// Delete the stored token. Not an error when there was none.
    pub fn forget() -> std::io::Result<()> {
        if !cfg!(target_os = "macos") {
            return Ok(());
        }
        Command::new("security")
            .args(["delete-generic-password", "-s", SERVICE, "-a", ACCOUNT])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|_| ())
    }
}

/// GitHub's device authorization flow.
///
/// Two requests: one to get a code the reader types into their browser,
/// then the same poll until GitHub says yes, no, or too late. Both are
/// blocking, like the rest of the crate, and run on the background
/// executor.
pub mod device {
    use super::Token;
    use crate::{Error, Result};
    use serde::Deserialize;
    use std::time::Duration;

    /// Where a code is asked for.
    pub const CODE_URL: &str = "https://github.com/login/device/code";
    /// Where the token is polled for.
    pub const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
    /// What the app asks to be allowed to do: read and write repositories
    /// and their issues and pulls, read the inbox, read organisation
    /// membership so private organisation repositories list, and read and
    /// write projects. A token from `gh` may lack `project`; the projects
    /// row says so when GitHub refuses.
    pub const SCOPE: &str = "repo notifications read:org project";
    /// The grant type the poll names.
    const GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

    /// What the reader is shown.
    #[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
    pub struct DeviceCode {
        /// What the app polls with. Not shown.
        pub device_code: String,
        /// What the reader types.
        pub user_code: String,
        /// Where they type it.
        pub verification_uri: String,
        /// How long the code lives, in seconds.
        pub expires_in: u64,
        /// How often the app may poll, in seconds.
        pub interval: u64,
    }

    impl DeviceCode {
        /// How often to poll, as GitHub asked plus nothing.
        pub fn interval(&self) -> Duration {
            Duration::from_secs(self.interval.max(1))
        }
    }

    /// What one poll said.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Poll {
        /// The reader has not finished yet; ask again after the interval.
        Pending,
        /// Asked too often; add five seconds to the interval.
        SlowDown,
        /// Signed in.
        Granted(Token),
        /// The reader refused.
        Denied,
        /// The code lived out its time.
        Expired,
    }

    #[derive(Debug, Deserialize)]
    struct PollBody {
        #[serde(default)]
        access_token: Option<String>,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        error_description: Option<String>,
    }

    /// Ask GitHub for a code.
    pub fn start(client_id: &str) -> Result<DeviceCode> {
        let body = post(CODE_URL, &[("client_id", client_id), ("scope", SCOPE)])?;
        parse_start(&body)
    }

    /// Ask GitHub whether the reader has typed the code yet.
    pub fn poll(client_id: &str, device_code: &str) -> Result<Poll> {
        let body = post(
            TOKEN_URL,
            &[
                ("client_id", client_id),
                ("device_code", device_code),
                ("grant_type", GRANT),
            ],
        )?;
        parse_poll(&body)
    }

    fn post(url: &str, form: &[(&str, &str)]) -> Result<String> {
        let agent = ureq::Agent::new_with_defaults();
        let mut response = agent
            .post(url)
            .header("Accept", "application/json")
            .header("User-Agent", "e1")
            .send_form(form.iter().copied())
            .map_err(|error| Error::Transport(error.to_string()))?;
        response
            .body_mut()
            .read_to_string()
            .map_err(|error| Error::Transport(error.to_string()))
    }

    /// The code out of GitHub's answer, or GitHub's complaint.
    pub fn parse_start(body: &str) -> Result<DeviceCode> {
        if let Ok(code) = serde_json::from_str::<DeviceCode>(body) {
            return Ok(code);
        }
        let complaint: PollBody =
            serde_json::from_str(body).map_err(|error| Error::Decode(error.to_string()))?;
        Err(Error::Status {
            status: 400,
            path: CODE_URL.to_string(),
            message: complaint
                .error_description
                .or(complaint.error)
                .unwrap_or_default(),
        })
    }

    /// The state out of a poll's answer.
    pub fn parse_poll(body: &str) -> Result<Poll> {
        let body: PollBody =
            serde_json::from_str(body).map_err(|error| Error::Decode(error.to_string()))?;
        if let Some(token) = body.access_token.and_then(Token::new) {
            return Ok(Poll::Granted(token));
        }
        match body.error.as_deref() {
            Some("authorization_pending") => Ok(Poll::Pending),
            Some("slow_down") => Ok(Poll::SlowDown),
            Some("access_denied") => Ok(Poll::Denied),
            Some("expired_token") => Ok(Poll::Expired),
            other => Err(Error::Status {
                status: 400,
                path: TOKEN_URL.to_string(),
                message: body
                    .error_description
                    .or_else(|| other.map(str::to_string))
                    .unwrap_or_default(),
            }),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_started_flow_carries_what_the_reader_is_shown() {
            let code = parse_start(r#"{"device_code":"d","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#).unwrap();
            assert_eq!(code.user_code, "ABCD-1234");
            assert_eq!(code.interval(), Duration::from_secs(5));
        }

        #[test]
        fn a_refused_start_is_githubs_own_words() {
            let error = parse_start(r#"{"error":"unauthorized_client","error_description":"device flow is not enabled"}"#).unwrap_err();
            assert!(error.to_string().contains("device flow is not enabled"));
        }

        #[test]
        fn every_poll_answer_is_a_state_and_the_token_is_kept_out_of_debug() {
            assert_eq!(
                parse_poll(r#"{"error":"authorization_pending"}"#).unwrap(),
                Poll::Pending
            );
            assert_eq!(
                parse_poll(r#"{"error":"slow_down","interval":10}"#).unwrap(),
                Poll::SlowDown
            );
            assert_eq!(
                parse_poll(r#"{"error":"access_denied"}"#).unwrap(),
                Poll::Denied
            );
            assert_eq!(
                parse_poll(r#"{"error":"expired_token"}"#).unwrap(),
                Poll::Expired
            );
            let granted =
                parse_poll(r#"{"access_token":"gho_abc","token_type":"bearer","scope":"repo"}"#)
                    .unwrap();
            assert!(matches!(&granted, Poll::Granted(token) if token.secret() == "gho_abc"));
            assert!(!format!("{granted:?}").contains("gho_abc"));
            assert!(parse_poll(r#"{"error":"incorrect_device_code"}"#).is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_explicit_variable_wins_over_the_shared_ones() {
        let env = |name: &str| match name {
            "E1_GITHUB_TOKEN" => Some("mine".to_string()),
            "GITHUB_TOKEN" => Some("theirs".to_string()),
            _ => None,
        };
        let (token, source) = resolve(
            env,
            || panic!("keychain must not be asked"),
            || panic!("gh must not be asked"),
        )
        .unwrap();
        assert_eq!(token.secret(), "mine");
        assert_eq!(source, Source::Environment);
    }

    #[test]
    fn the_keychain_comes_before_gh() {
        let (token, source) = resolve(
            |_| None,
            || Some("kept\n".to_string()),
            || panic!("gh must not be asked"),
        )
        .unwrap();
        assert_eq!(token.secret(), "kept", "trimmed");
        assert_eq!(source, Source::Keychain);
    }

    #[test]
    fn gh_is_only_asked_when_nothing_else_had_a_token() {
        let (token, source) = resolve(|_| None, || None, || Some("from-gh".to_string())).unwrap();
        assert_eq!(token.secret(), "from-gh");
        assert_eq!(source, Source::Gh);
    }

    #[test]
    fn an_empty_variable_is_the_same_as_an_unset_one() {
        let env = |name: &str| (name == "GITHUB_TOKEN").then(|| "   ".to_string());
        assert!(resolve(env, || None, || None).is_none());
    }

    #[test]
    fn the_token_does_not_appear_in_debug_output() {
        let token = Token::new("ghp_secret").unwrap();
        assert!(!format!("{token:?}").contains("secret"));
    }

    #[test]
    fn there_is_always_an_app_to_sign_in_as() {
        // The environment may or may not set one; either way the answer is
        // never empty, so the sign-in screen always has somewhere to go.
        assert!(!client_id().trim().is_empty());
        assert!(!DEFAULT_CLIENT_ID.trim().is_empty());
    }

    #[test]
    fn the_keychain_refuses_a_token_it_could_not_quote() {
        // Never reaches `security`: the check comes first.
        let token = Token::new("bad token\"; rm").unwrap();
        assert!(Keychain::store(&token).is_err());
    }
}
