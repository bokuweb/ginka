//! Something being loaded, with what was there before.
//!
//! Every list and every detail is one of these. The rule it encodes is
//! stale-while-revalidate: a refresh keeps the value on screen until the
//! fresh one lands, and a failure keeps it too, so the window never blanks
//! a list the reader was looking at because GitHub blinked.

/// The state of one fetch.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Fetch<T> {
    /// Never asked.
    #[default]
    Idle,
    /// Asked, not yet answered. `stale` is what was on screen before.
    Loading {
        /// The previous value, kept on screen.
        stale: Option<T>,
    },
    /// Answered.
    Ready(T),
    /// Answered with an error. `stale` is what was on screen before.
    Failed {
        /// Why, as the reader will see it.
        error: String,
        /// The previous value, kept on screen.
        stale: Option<T>,
    },
}

impl<T> Fetch<T> {
    /// Start a fetch, keeping whatever is on screen as stale.
    pub fn begin(&mut self) {
        let stale = std::mem::take(self).into_value();
        *self = Self::Loading { stale };
    }

    /// Finish a fetch. A failure keeps the stale value.
    pub fn finish(&mut self, result: Result<T, String>) {
        let stale = std::mem::take(self).into_value();
        *self = match result {
            Ok(value) => Self::Ready(value),
            Err(error) => Self::Failed { error, stale },
        };
    }

    /// Whatever is showable: the fresh value, or the stale one.
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Idle => None,
            Self::Loading { stale } | Self::Failed { stale, .. } => stale.as_ref(),
            Self::Ready(value) => Some(value),
        }
    }

    /// Whether a fetch is in flight.
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading { .. })
    }

    /// The last failure, if the fetch ended in one.
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Failed { error, .. } => Some(error),
            _ => None,
        }
    }

    /// Whether nothing has ever been asked, which is when a view asks.
    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    fn into_value(self) -> Option<T> {
        match self {
            Self::Idle => None,
            Self::Loading { stale } | Self::Failed { stale, .. } => stale,
            Self::Ready(value) => Some(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refresh_keeps_the_old_value_on_screen_until_the_new_one_lands() {
        let mut fetch = Fetch::Ready(vec![1]);
        fetch.begin();
        assert!(fetch.is_loading());
        assert_eq!(fetch.value(), Some(&vec![1]));
        fetch.finish(Ok(vec![1, 2]));
        assert_eq!(fetch.value(), Some(&vec![1, 2]));
        assert!(!fetch.is_loading());
    }

    #[test]
    fn a_failure_keeps_the_old_value_and_says_why() {
        let mut fetch = Fetch::Ready(vec![1]);
        fetch.begin();
        fetch.finish(Err("offline".into()));
        assert_eq!(fetch.value(), Some(&vec![1]));
        assert_eq!(fetch.error(), Some("offline"));
        // And a failure after a failure still remembers the value.
        fetch.begin();
        fetch.finish(Err("still offline".into()));
        assert_eq!(fetch.value(), Some(&vec![1]));
    }

    #[test]
    fn the_first_fetch_has_nothing_to_show() {
        let mut fetch: Fetch<Vec<u8>> = Fetch::Idle;
        assert!(fetch.is_idle());
        fetch.begin();
        assert_eq!(fetch.value(), None);
        assert!(!fetch.is_idle());
    }
}

/// What the reader sees when a fetch fails.
///
/// The two errors that have a remedy get their own sentence; the rest are
/// GitHub's own words behind a prefix, because "could not reach GitHub" plus
/// the transport's message is more useful than either alone.
pub fn describe(error: &e1_github::Error) -> String {
    use e1_github::Error;
    match error {
        Error::NoToken => rust_i18n::t!("error.no_token").to_string(),
        Error::RateLimited { .. } => rust_i18n::t!("error.rate_limited").to_string(),
        other => rust_i18n::t!("error.failed", detail = other.to_string()).to_string(),
    }
}

#[cfg(test)]
mod describe_tests {
    use super::*;

    #[test]
    fn a_missing_token_says_how_to_get_one() {
        rust_i18n::set_locale("en");
        assert!(describe(&e1_github::Error::NoToken).contains("gh auth login"));
        assert!(describe(&e1_github::Error::Transport("dns".into())).contains("dns"));
    }
}
