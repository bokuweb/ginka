//! The reader's own keyboard shortcuts — MonoCode's Settings → Keybindings,
//! as a file: `keymap.json` in Ginka's state directory.
//!
//! ```json
//! [
//!   { "keys": "cmd-shift-b", "action": "toggle_sidebar" },
//!   { "keys": "cmd-b", "action": null }
//! ]
//! ```
//!
//! Each entry binds `keys` to one of [`ACTIONS`], or with `null` frees
//! `keys` from whatever it did. Entries are applied after the defaults, so
//! they win. A file that binds one chord twice, names an action this build
//! does not have, or cannot be read is refused whole — a half-applied
//! keymap is harder to reason about than the defaults.

use serde::Deserialize;

/// The actions a keymap may bind, by the name the file uses.
pub const ACTIONS: &[&str] = &[
    "toggle_sidebar",
    "toggle_right_panel",
    "toggle_terminal_dock",
    "toggle_palette",
    "search_everywhere",
    "find_transcript",
    "next_surface",
    "previous_surface",
    "new_tab",
    "close_tab",
    "close_all_tabs",
    "next_tab",
    "previous_tab",
    "next_session",
    "previous_session",
    "navigate_back",
    "navigate_forward",
    "open_settings",
    "open_board",
];

/// One entry of the file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Override {
    /// A GPUI keystroke sequence: `cmd-shift-b`, `ctrl-k ctrl-s`.
    pub keys: String,
    /// One of [`ACTIONS`], or `None` to unbind `keys`.
    pub action: Option<String>,
}

/// Read a keymap file's text. An empty file is no overrides.
pub fn parse(text: &str) -> Result<Vec<Override>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let overrides: Vec<Override> =
        serde_json::from_str(text).map_err(|error| format!("keymap.json is not valid: {error}"))?;
    let mut seen = std::collections::HashSet::new();
    for entry in &overrides {
        let keys = entry.keys.split_whitespace().collect::<Vec<_>>().join(" ");
        if keys.is_empty() {
            return Err("a keymap entry has no keys".into());
        }
        if !seen.insert(keys.clone()) {
            return Err(format!("`{keys}` is bound twice in keymap.json"));
        }
        if let Some(action) = &entry.action
            && !ACTIONS.contains(&action.as_str())
        {
            return Err(format!(
                "`{action}` is not an action a keymap can bind; known: {}",
                ACTIONS.join(", ")
            ));
        }
    }
    Ok(overrides)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keymap_rebinds_and_unbinds() {
        let parsed = parse(
            r#"[{"keys": "cmd-shift-b", "action": "toggle_sidebar"},
                {"keys": "cmd-b", "action": null}]"#,
        )
        .unwrap();
        assert_eq!(
            parsed,
            vec![
                Override {
                    keys: "cmd-shift-b".into(),
                    action: Some("toggle_sidebar".into())
                },
                Override {
                    keys: "cmd-b".into(),
                    action: None
                },
            ]
        );
        assert_eq!(parse("  \n").unwrap(), Vec::new());
    }

    #[test]
    fn conflicts_unknown_actions_and_bad_files_are_refused_whole() {
        let twice = parse(
            r#"[{"keys": "cmd-b", "action": "toggle_sidebar"},
                {"keys": "cmd-b", "action": "toggle_palette"}]"#,
        );
        assert!(twice.unwrap_err().contains("bound twice"));
        let unknown = parse(r#"[{"keys": "cmd-b", "action": "format_disk"}]"#);
        assert!(unknown.unwrap_err().contains("not an action"));
        assert!(parse(r#"[{"keys": "  ", "action": null}]"#).is_err());
        assert!(parse("{not json").is_err());
    }
}
