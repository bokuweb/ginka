//! The command palette: every action reachable by typing its name.
//!
//! `docs/ui.md` §6 asks that no action be mouse-only, which is a promise about
//! this list rather than about any one view: an action that is not here is one
//! a keyboard cannot reach. The entries are built from what the window
//! currently is — an open panel offers to close — so the palette never offers
//! something that would do nothing.
//!
//! Matching is `nucleo`, the same matcher as the file finder, so a palette and
//! a quick-open behave the same way under the same typing.

use crate::layout::{Layout, Panel};
use crate::surface::Surface;
use crate::workspace::SessionRow;
use ginka_protocol::WorkspaceId;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

/// What choosing an entry does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Open or close one of the window's panels.
    TogglePanel(Panel),
    /// Show a surface in the right panel, opening it if it is closed.
    ShowSurface(Surface),
    /// Start another shell in the dock.
    NewTerminal,
    /// Split the active terminal pane, or return to one pane.
    ToggleTerminalSplit,
    /// Move keyboard input to the other visible terminal pane.
    FocusOtherTerminalPane,
    /// Find text in the active terminal's bounded history.
    FindTerminal,
    /// Move to the next terminal tab.
    NextTerminal,
    /// Move to the previous terminal tab.
    PreviousTerminal,
    /// Return the active terminal from history to live output.
    TerminalToLive,
    /// Ask to close the active daemon-owned terminal.
    CloseTerminal,
    /// Build or refresh semantic search for the selected workspace.
    IndexWorkspace,
    /// Choose files for the next prompt.
    AttachFiles,
    /// Find persisted text in the open conversation.
    FindTranscript,
    /// Show or hide the open conversation's prompt outline.
    TogglePromptOutline,
    /// Return to the previous project or session visit.
    NavigateBack,
    /// Return to the next project or session visit.
    NavigateForward,
    /// Select a workspace in the sidebar.
    Switch(WorkspaceId),
}

/// Which contextual terminal actions can do something in the current window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalActions {
    /// Number of daemon terminals shown in the shared tab strip.
    pub tabs: usize,
    /// Whether two terminal panes are currently visible.
    pub split: bool,
    /// Whether the active terminal is browsing older output.
    pub browsing_history: bool,
    /// Whether closing the active terminal is awaiting confirmation.
    pub close_armed: bool,
}

/// Terminal commands that are meaningful for the current tab and pane state.
pub fn terminal_entries(state: TerminalActions) -> Vec<Entry> {
    if state.tabs == 0 {
        return Vec::new();
    }
    let mut entries = vec![
        Entry {
            id: "terminal:split".into(),
            label: if state.split {
                rust_i18n::t!("terminal.split.close").to_string()
            } else {
                rust_i18n::t!("terminal.split.open").to_string()
            },
            hint: None,
            command: Command::ToggleTerminalSplit,
        },
        Entry {
            id: "terminal:find".into(),
            label: rust_i18n::t!("terminal.search.open").to_string(),
            hint: Some(terminal_find_shortcut().into()),
            command: Command::FindTerminal,
        },
        Entry {
            id: "terminal:close".into(),
            label: if state.close_armed {
                rust_i18n::t!("terminal.close.short").to_string()
            } else {
                rust_i18n::t!("terminal.close").to_string()
            },
            hint: None,
            command: Command::CloseTerminal,
        },
    ];
    if state.split {
        entries.push(Entry {
            id: "terminal:focus-other".into(),
            label: rust_i18n::t!("terminal.split.focus_other").to_string(),
            hint: None,
            command: Command::FocusOtherTerminalPane,
        });
    }
    if state.tabs > 1 {
        entries.extend([
            Entry {
                id: "terminal:next".into(),
                label: rust_i18n::t!("palette.terminal.next").to_string(),
                hint: Some(next_terminal_shortcut().into()),
                command: Command::NextTerminal,
            },
            Entry {
                id: "terminal:previous".into(),
                label: rust_i18n::t!("palette.terminal.previous").to_string(),
                hint: Some(previous_terminal_shortcut().into()),
                command: Command::PreviousTerminal,
            },
        ]);
    }
    if state.browsing_history {
        entries.push(Entry {
            id: "terminal:live".into(),
            label: rust_i18n::t!("terminal.history.live.tooltip").to_string(),
            hint: None,
            command: Command::TerminalToLive,
        });
    }
    entries
}

#[cfg(target_os = "macos")]
fn terminal_find_shortcut() -> &'static str {
    "⌘F"
}

#[cfg(not(target_os = "macos"))]
fn terminal_find_shortcut() -> &'static str {
    "Ctrl+F"
}

#[cfg(target_os = "macos")]
fn next_terminal_shortcut() -> &'static str {
    "⌘⇧]"
}

#[cfg(not(target_os = "macos"))]
fn next_terminal_shortcut() -> &'static str {
    "Ctrl+PageDown"
}

#[cfg(target_os = "macos")]
fn previous_terminal_shortcut() -> &'static str {
    "⌘⇧["
}

#[cfg(not(target_os = "macos"))]
fn previous_terminal_shortcut() -> &'static str {
    "Ctrl+PageUp"
}

/// One line of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Stable across a filter, so a view can key an element by it.
    pub id: String,
    pub label: String,
    /// The keystroke that does the same thing, or where the entry leads.
    pub hint: Option<String>,
    pub command: Command,
}

/// Everything the palette can do, given the window as it currently is.
///
/// Workspaces come last: they are the longest run and the one the user is most
/// likely to reach by typing a name rather than by reading the list.
pub fn entries(
    layout: &Layout,
    rows: &[SessionRow],
    workspace_indexed: Option<bool>,
    session_open: bool,
    can_go_back: bool,
    can_go_forward: bool,
) -> Vec<Entry> {
    let mut all = Vec::new();
    for panel in Panel::ALL {
        let open = layout.is_open(*panel);
        all.push(Entry {
            id: format!("panel:{}", panel.label()),
            label: if open {
                rust_i18n::t!("palette.panel.close", panel = panel.label()).to_string()
            } else {
                rust_i18n::t!("palette.panel.open", panel = panel.label()).to_string()
            },
            hint: Some(shortcut(*panel).to_string()),
            command: Command::TogglePanel(*panel),
        });
    }
    for surface in Surface::ALL {
        all.push(Entry {
            id: format!("surface:{}", surface.label()),
            label: rust_i18n::t!("palette.surface", surface = surface.label()).to_string(),
            hint: None,
            command: Command::ShowSurface(*surface),
        });
    }
    if can_go_back {
        all.push(Entry {
            id: "navigation:back".into(),
            label: rust_i18n::t!("palette.navigation.back").to_string(),
            hint: Some("⌘[".into()),
            command: Command::NavigateBack,
        });
    }
    if can_go_forward {
        all.push(Entry {
            id: "navigation:forward".into(),
            label: rust_i18n::t!("palette.navigation.forward").to_string(),
            hint: Some("⌘]".into()),
            command: Command::NavigateForward,
        });
    }
    all.push(Entry {
        id: "terminal:new".into(),
        label: rust_i18n::t!("palette.terminal.new").to_string(),
        hint: None,
        command: Command::NewTerminal,
    });
    all.push(Entry {
        id: "composer:attach".into(),
        label: rust_i18n::t!("palette.composer.attach").to_string(),
        hint: None,
        command: Command::AttachFiles,
    });
    if let Some(indexed) = workspace_indexed {
        all.push(Entry {
            id: "workspace:index".into(),
            label: if indexed {
                rust_i18n::t!("palette.workspace.reindex").to_string()
            } else {
                rust_i18n::t!("palette.workspace.index").to_string()
            },
            hint: None,
            command: Command::IndexWorkspace,
        });
    }
    if session_open {
        all.push(Entry {
            id: "transcript:find".into(),
            label: rust_i18n::t!("palette.transcript.find").to_string(),
            hint: Some(rust_i18n::t!("palette.transcript.find.hint").to_string()),
            command: Command::FindTranscript,
        });
        all.push(Entry {
            id: "transcript:outline".into(),
            label: rust_i18n::t!("palette.transcript.outline").to_string(),
            hint: None,
            command: Command::TogglePromptOutline,
        });
    }
    for row in rows {
        all.push(Entry {
            id: format!("workspace:{}", row.workspace.0),
            label: rust_i18n::t!("palette.switch", workspace = row.title.to_string()).to_string(),
            hint: Some(row.branch.to_string()),
            command: Command::Switch(row.workspace.clone()),
        });
    }
    all
}

/// The entries that match `query`, best first.
///
/// An empty query is the whole list in the order it was built: a palette that
/// shuffled itself before anything was typed would be unreadable.
pub fn filter(entries: Vec<Entry>, query: &str) -> Vec<Entry> {
    let query = query.trim();
    if query.is_empty() {
        return entries;
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    // Matched against the label rather than the id: the id is ours, and the
    // label is what the reader can see themselves typing part of.
    let mut scored: Vec<(u32, Entry)> = entries
        .into_iter()
        .filter_map(|entry| {
            let mut buffer = Vec::new();
            let haystack = nucleo_matcher::Utf32Str::new(&entry.label, &mut buffer);
            pattern
                .score(haystack, &mut matcher)
                .map(|score| (score, entry.clone()))
        })
        .collect();
    // Stable on the score and then on the label, so the same query always
    // answers with the same list.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.label.cmp(&b.1.label)));
    scored.into_iter().map(|(_, entry)| entry).collect()
}

/// The keystroke that toggles a panel, as `docs/ui.md` §6 assigns them.
fn shortcut(panel: Panel) -> &'static str {
    match panel {
        Panel::Sidebar => "⌘B",
        Panel::RightPanel => "⌘⌥B",
        Panel::TerminalDock => "⌘J",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ginka_core::settings::AppSettings;

    fn layout() -> Layout {
        Layout::from_settings(&AppSettings::default())
    }

    #[test]
    fn an_open_panel_offers_to_close_and_a_closed_one_to_open() {
        // An entry that would do nothing is worse than no entry: the reader
        // has to try it to find out.
        let mut layout = layout();
        let opened = entries(&layout, &[], None, false, false, false);
        let sidebar = opened
            .iter()
            .find(|entry| entry.command == Command::TogglePanel(Panel::Sidebar))
            .unwrap();
        let before = sidebar.label.clone();

        layout.toggle(Panel::Sidebar);
        let after = entries(&layout, &[], None, false, false, false);
        let sidebar = after
            .iter()
            .find(|entry| entry.command == Command::TogglePanel(Panel::Sidebar))
            .unwrap();
        assert_ne!(before, sidebar.label);
    }

    #[test]
    fn every_surface_and_panel_is_reachable_by_typing() {
        let all = entries(&layout(), &[], None, false, false, false);
        for panel in Panel::ALL {
            assert!(
                all.iter()
                    .any(|entry| entry.command == Command::TogglePanel(*panel)),
                "{panel:?} is mouse-only without an entry"
            );
        }
        for surface in Surface::ALL {
            assert!(
                all.iter()
                    .any(|entry| entry.command == Command::ShowSurface(*surface))
            );
        }
    }

    #[test]
    fn history_commands_are_offered_only_when_they_can_move() {
        let still = entries(&layout(), &[], None, false, false, false);
        assert!(!still.iter().any(|entry| matches!(
            entry.command,
            Command::NavigateBack | Command::NavigateForward
        )));

        let backwards = entries(&layout(), &[], None, false, true, false);
        assert!(
            backwards
                .iter()
                .any(|entry| entry.command == Command::NavigateBack)
        );
        assert!(
            !backwards
                .iter()
                .any(|entry| entry.command == Command::NavigateForward)
        );
    }

    #[test]
    fn typing_part_of_a_name_finds_the_entry() {
        let all = entries(&layout(), &[], None, false, false, false);
        let found = filter(all, "termi");
        assert!(
            found
                .iter()
                .take(3)
                .any(|entry| entry.command == Command::NewTerminal
                    || entry.command == Command::ShowSurface(Surface::Terminal)),
            "{:?}",
            found.iter().map(|e| &e.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn nothing_typed_is_the_whole_list_in_its_own_order() {
        let all = entries(&layout(), &[], None, false, false, false);
        assert_eq!(filter(all.clone(), "   "), all);
    }

    #[test]
    fn a_query_that_matches_nothing_answers_with_nothing() {
        let all = entries(&layout(), &[], None, false, false, false);
        assert!(filter(all, "zzzzzzzz").is_empty());
    }

    #[test]
    fn indexing_is_offered_only_for_a_workspace_and_changes_word_when_ready() {
        let without_workspace = entries(&layout(), &[], None, false, false, false);
        assert!(
            !without_workspace
                .iter()
                .any(|entry| entry.command == Command::IndexWorkspace)
        );

        let unindexed = entries(&layout(), &[], Some(false), false, false, false);
        let index = unindexed
            .iter()
            .find(|entry| entry.command == Command::IndexWorkspace)
            .unwrap();
        let indexed = entries(&layout(), &[], Some(true), false, false, false);
        let reindex = indexed
            .iter()
            .find(|entry| entry.command == Command::IndexWorkspace)
            .unwrap();
        assert_ne!(index.label, reindex.label);
    }

    #[test]
    fn conversation_search_is_offered_only_when_a_session_is_open() {
        assert!(
            !entries(&layout(), &[], None, false, false, false)
                .iter()
                .any(|entry| entry.command == Command::FindTranscript)
        );
        assert!(
            entries(&layout(), &[], None, true, false, false)
                .iter()
                .any(|entry| entry.command == Command::FindTranscript)
        );
        assert!(
            !entries(&layout(), &[], None, false, false, false)
                .iter()
                .any(|entry| entry.command == Command::TogglePromptOutline)
        );
        assert!(
            entries(&layout(), &[], None, true, false, false)
                .iter()
                .any(|entry| entry.command == Command::TogglePromptOutline)
        );
    }

    #[test]
    fn attaching_files_is_available_before_a_session_exists() {
        assert!(
            entries(&layout(), &[], None, false, false, false)
                .iter()
                .any(|entry| entry.command == Command::AttachFiles)
        );
    }

    #[test]
    fn terminal_palette_actions_follow_the_visible_terminal_state() {
        assert!(terminal_entries(TerminalActions::default()).is_empty());

        let one = terminal_entries(TerminalActions {
            tabs: 1,
            browsing_history: true,
            ..TerminalActions::default()
        });
        for command in [
            Command::ToggleTerminalSplit,
            Command::FindTerminal,
            Command::TerminalToLive,
            Command::CloseTerminal,
        ] {
            assert!(
                one.iter().any(|entry| entry.command == command),
                "{command:?} is not reachable from the palette"
            );
        }
        assert!(!one.iter().any(|entry| matches!(
            entry.command,
            Command::FocusOtherTerminalPane | Command::NextTerminal | Command::PreviousTerminal
        )));
        let unarmed_close = one
            .iter()
            .find(|entry| entry.command == Command::CloseTerminal)
            .map(|entry| entry.label.clone())
            .unwrap();

        let split = terminal_entries(TerminalActions {
            tabs: 3,
            split: true,
            close_armed: true,
            ..TerminalActions::default()
        });
        for command in [
            Command::FocusOtherTerminalPane,
            Command::NextTerminal,
            Command::PreviousTerminal,
        ] {
            assert!(split.iter().any(|entry| entry.command == command));
        }
        let armed_close = split
            .iter()
            .find(|entry| entry.command == Command::CloseTerminal)
            .map(|entry| entry.label.clone())
            .unwrap();
        assert_ne!(
            armed_close, unarmed_close,
            "the destructive second activation must identify itself"
        );
        assert!(
            !split
                .iter()
                .any(|entry| entry.command == Command::TerminalToLive)
        );
    }
}
