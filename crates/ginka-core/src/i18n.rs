//! Which language the app speaks.
//!
//! `en` and `ja` are both maintained from the first release, because
//! retrofitting localization is expensive (`docs/roadmap.md` §6.4). The strings
//! themselves live in `locales/app.yml`, with both languages side by side so a
//! missing translation is visible in review rather than in a running window.
//!
//! Choosing the language lives here rather than in `ginka-ui` because the CLI
//! prints to a human too, and it must not link a UI toolkit to know what
//! language that human reads. Each crate that shows a string loads the
//! catalogue itself; the *current* locale is one global in `rust_i18n`, so
//! setting it here sets it for all of them.

/// The language to use, in order of authority.
///
/// An explicit choice in `app.json` wins. Failing that the environment is
/// asked, because a user whose desktop is in Japanese has already said what
/// they want. Anything unrecognised falls back to English rather than to an
/// empty window.
pub fn resolve(preference: Option<&str>, environment: Option<&str>) -> String {
    preference
        .and_then(normalize)
        .or_else(|| environment.and_then(normalize))
        .unwrap_or_else(|| "en".to_string())
}

/// Reduce a locale tag to one this app has strings for.
///
/// `LANG` arrives as `ja_JP.UTF-8` and a BCP-47 preference as `ja-JP`; both are
/// the same language to us. `C` and `POSIX` mean "no preference stated", not a
/// language.
fn normalize(tag: &str) -> Option<String> {
    let language = tag
        .split(['_', '.', '-', '@'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match language.as_str() {
        "ja" => Some("ja".to_string()),
        "en" => Some("en".to_string()),
        _ => None,
    }
}

/// The locale the environment asks for, if it says.
pub fn from_environment() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|value| !value.is_empty())
}

/// Set the language for every string in this process.
pub fn apply(locale: &str) {
    rust_i18n::set_locale(locale);
}

/// Resolve and apply in one step, and report what was chosen.
pub fn init(preference: Option<&str>) -> String {
    let locale = resolve(preference, from_environment().as_deref());
    apply(&locale);
    locale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_choice_wins_over_the_environment() {
        assert_eq!(resolve(Some("ja"), Some("en_US.UTF-8")), "ja");
    }

    #[test]
    fn the_environment_is_used_when_nothing_was_chosen() {
        assert_eq!(resolve(None, Some("ja_JP.UTF-8")), "ja");
        assert_eq!(resolve(None, Some("en-GB")), "en");
    }

    #[test]
    fn a_language_this_build_has_no_strings_for_falls_back_to_english() {
        assert_eq!(resolve(Some("fr"), Some("de_DE.UTF-8")), "en");
        assert_eq!(resolve(None, None), "en");
    }

    #[test]
    fn the_posix_locale_is_not_a_language() {
        // `LANG=C` is the absence of a preference; reading it as one would
        // pick a language nobody asked for.
        assert_eq!(resolve(None, Some("C")), "en");
        assert_eq!(resolve(None, Some("POSIX")), "en");
    }

    /// The catalogue, checked as text so this needs no YAML parser.
    const CATALOGUE: &str = include_str!("../../../locales/app.yml");

    #[test]
    fn every_string_is_translated_into_both_languages() {
        // A key with only one language would render as its own name in the
        // other; that is the failure this catches before a release does.
        let mut key: Option<&str> = None;
        let mut languages: Vec<&str> = Vec::new();
        let mut missing: Vec<String> = Vec::new();

        let close = |key: &Option<&str>, languages: &Vec<&str>, missing: &mut Vec<String>| {
            if let Some(key) = key {
                for language in ["en", "ja"] {
                    if !languages.contains(&language) {
                        missing.push(format!("{key} has no {language}"));
                    }
                }
            }
        };

        for line in CATALOGUE.lines() {
            if line.starts_with('#') || line.trim().is_empty() || line.starts_with("_version") {
                continue;
            }
            if !line.starts_with(' ') {
                close(&key, &languages, &mut missing);
                key = line.split(':').next();
                languages.clear();
            } else if let Some(language) = line.trim().split(':').next() {
                languages.push(language);
            }
        }
        close(&key, &languages, &mut missing);

        assert!(missing.is_empty(), "{missing:?}");
    }

    #[test]
    fn the_catalogue_is_the_version_that_keeps_the_languages_together() {
        assert!(
            CATALOGUE.contains("_version: 2"),
            "side-by-side languages are what makes a gap reviewable"
        );
    }
}
