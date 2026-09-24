//! The conversations open across the top of the centre column.
//!
//! MonoCode's tab strip (`docs/parity.md` §3): the sessions list is where a
//! conversation is found, and a tab is where it is kept while the reader moves
//! between two or three of them. A tab is a workspace — the conversation the
//! centre column shows is the one running in it — plus the one tab that has
//! no workspace yet, the new chat the next prompt will start.

use ginka_protocol::WorkspaceId;

/// One tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tab {
    /// A new conversation that has not been sent. At most one: two empty
    /// composers side by side is one too many places to type.
    New,
    /// The conversation in a workspace.
    Workspace(WorkspaceId),
}

/// The open tabs, left to right, and which one is showing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tabs {
    tabs: Vec<Tab>,
    active: Option<usize>,
}

impl Tabs {
    /// Tabs restored from settings, with none active until the window picks.
    pub fn restore(workspaces: impl IntoIterator<Item = WorkspaceId>) -> Self {
        let mut tabs = Self::default();
        for workspace in workspaces {
            let tab = Tab::Workspace(workspace);
            if !tabs.tabs.contains(&tab) {
                tabs.tabs.push(tab);
            }
        }
        tabs
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn active(&self) -> Option<&Tab> {
        self.tabs.get(self.active?)
    }

    pub fn active_index(&self) -> Option<usize> {
        self.active
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Show a tab, opening it after the active one if it is not open. Opening
    /// the new-chat tab reuses the one there is.
    pub fn open(&mut self, tab: Tab) {
        if let Some(index) = self.tabs.iter().position(|open| open == &tab) {
            self.active = Some(index);
            return;
        }
        let at = self
            .active
            .map(|index| index + 1)
            .unwrap_or(self.tabs.len());
        self.tabs.insert(at, tab);
        self.active = Some(at);
    }

    /// The new-chat tab became a conversation: it keeps its place in the
    /// strip rather than closing and reopening somewhere else.
    pub fn promote_new(&mut self, workspace: WorkspaceId) {
        let tab = Tab::Workspace(workspace);
        if let Some(existing) = self.tabs.iter().position(|open| open == &tab) {
            // Already open elsewhere: the new chat folds into it.
            if let Some(new) = self.tabs.iter().position(|open| open == &Tab::New) {
                self.tabs.remove(new);
            }
            let existing = self
                .tabs
                .iter()
                .position(|open| open == &tab)
                .unwrap_or(existing);
            self.active = Some(existing);
            return;
        }
        match self.tabs.iter().position(|open| open == &Tab::New) {
            Some(index) => {
                self.tabs[index] = tab;
                self.active = Some(index);
            }
            None => self.open(tab),
        }
    }

    /// Close a tab. The one to its right takes over, or the one to its left
    /// when it was the last — the neighbour the eye is already on.
    pub fn close(&mut self, index: usize) -> Option<Tab> {
        if index >= self.tabs.len() {
            return None;
        }
        let closed = self.tabs.remove(index);
        self.active = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(active) if active > index => Some(active - 1),
            Some(active) if active == index => Some(index.min(self.tabs.len() - 1)),
            other => other,
        };
        Some(closed)
    }

    /// Close every tab whose workspace is gone.
    pub fn retain_workspaces(&mut self, exists: impl Fn(&WorkspaceId) -> bool) {
        let mut index = 0;
        while index < self.tabs.len() {
            match &self.tabs[index] {
                Tab::Workspace(workspace) if !exists(workspace) => {
                    self.close(index);
                }
                _ => index += 1,
            }
        }
    }

    /// `⌘1..8`: the tab at a position; `⌘9` is always the last.
    pub fn select_number(&mut self, number: usize) -> Option<&Tab> {
        if self.tabs.is_empty() || number == 0 {
            return None;
        }
        let index = if number >= 9 {
            self.tabs.len() - 1
        } else {
            number - 1
        };
        if index >= self.tabs.len() {
            return None;
        }
        self.active = Some(index);
        self.active()
    }

    /// `⇧⌘]` and `⇧⌘[`: the next or previous tab, wrapping.
    pub fn cycle(&mut self, forward: bool) -> Option<&Tab> {
        let count = self.tabs.len();
        if count == 0 {
            return None;
        }
        let at = self.active.unwrap_or(0);
        self.active = Some(if forward {
            (at + 1) % count
        } else {
            (at + count - 1) % count
        });
        self.active()
    }

    /// What to write back to settings: the workspaces, in order.
    pub fn workspaces(&self) -> Vec<WorkspaceId> {
        self.tabs
            .iter()
            .filter_map(|tab| match tab {
                Tab::Workspace(workspace) => Some(workspace.clone()),
                Tab::New => None,
            })
            .collect()
    }
}

/// Which workspace to show when the window opens: the one on screen when
/// it closed, if it is still there, else the first open tab that is.
/// `None` leaves the home screen up.
pub fn restore_selection(
    last: Option<&str>,
    open_tabs: &[WorkspaceId],
    live: &[WorkspaceId],
) -> Option<WorkspaceId> {
    last.map(|last| WorkspaceId(last.to_string()))
        .into_iter()
        .chain(open_tabs.iter().cloned())
        .find(|candidate| live.contains(candidate))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_window_opens_where_it_was_left_or_on_the_first_tab_still_there() {
        let id = |text: &str| WorkspaceId(text.into());
        let live = [id("comet/main"), id("comet/try-1")];
        assert_eq!(
            restore_selection(Some("comet/try-1"), &[id("comet/main")], &live),
            Some(id("comet/try-1"))
        );
        assert_eq!(
            restore_selection(
                Some("comet/gone"),
                &[id("comet/gone"), id("comet/main")],
                &live
            ),
            Some(id("comet/main")),
            "a workspace that is gone is skipped"
        );
        assert_eq!(restore_selection(None, &[], &live), None, "the home screen");
        assert_eq!(restore_selection(Some("comet/gone"), &[], &live), None);
    }

    use super::*;

    fn ws(name: &str) -> WorkspaceId {
        WorkspaceId(format!("p/{name}"))
    }

    #[test]
    fn a_tab_opens_beside_the_one_you_are_on_and_is_never_open_twice() {
        let mut tabs = Tabs::default();
        tabs.open(Tab::Workspace(ws("a")));
        tabs.open(Tab::Workspace(ws("b")));
        tabs.select_number(1);
        tabs.open(Tab::Workspace(ws("c")));
        assert_eq!(tabs.workspaces(), vec![ws("a"), ws("c"), ws("b")]);
        tabs.open(Tab::Workspace(ws("b")));
        assert_eq!(tabs.tabs().len(), 3);
        assert_eq!(tabs.active(), Some(&Tab::Workspace(ws("b"))));
    }

    #[test]
    fn closing_hands_over_to_the_right_then_the_left() {
        let mut tabs = Tabs::restore([ws("a"), ws("b"), ws("c")]);
        tabs.select_number(2);
        tabs.close(1);
        assert_eq!(tabs.active(), Some(&Tab::Workspace(ws("c"))));
        tabs.close(1);
        assert_eq!(tabs.active(), Some(&Tab::Workspace(ws("a"))));
        tabs.close(0);
        assert_eq!(tabs.active(), None);
        assert!(tabs.is_empty());
    }

    #[test]
    fn closing_a_tab_left_of_the_active_one_keeps_the_active_one() {
        let mut tabs = Tabs::restore([ws("a"), ws("b"), ws("c")]);
        tabs.select_number(3);
        tabs.close(0);
        assert_eq!(tabs.active(), Some(&Tab::Workspace(ws("c"))));
    }

    #[test]
    fn a_new_chat_becomes_its_conversation_in_place() {
        let mut tabs = Tabs::restore([ws("a")]);
        tabs.select_number(1);
        tabs.open(Tab::New);
        tabs.open(Tab::New);
        assert_eq!(tabs.tabs().len(), 2, "one new-chat tab at most");
        tabs.promote_new(ws("b"));
        assert_eq!(
            tabs.tabs(),
            &[Tab::Workspace(ws("a")), Tab::Workspace(ws("b"))]
        );
        assert_eq!(tabs.active_index(), Some(1));
    }

    #[test]
    fn a_new_chat_that_lands_in_an_open_workspace_folds_into_its_tab() {
        let mut tabs = Tabs::restore([ws("a")]);
        tabs.open(Tab::New);
        tabs.promote_new(ws("a"));
        assert_eq!(tabs.tabs(), &[Tab::Workspace(ws("a"))]);
        assert_eq!(tabs.active_index(), Some(0));
    }

    #[test]
    fn numbers_and_cycling_follow_the_browser_chords() {
        let mut tabs = Tabs::restore([ws("a"), ws("b"), ws("c")]);
        assert_eq!(tabs.select_number(9), Some(&Tab::Workspace(ws("c"))));
        assert_eq!(tabs.select_number(5), None);
        assert_eq!(tabs.cycle(true), Some(&Tab::Workspace(ws("a"))));
        assert_eq!(tabs.cycle(false), Some(&Tab::Workspace(ws("c"))));
    }

    #[test]
    fn a_removed_workspace_takes_its_tab_with_it() {
        let mut tabs = Tabs::restore([ws("a"), ws("b")]);
        tabs.open(Tab::New);
        tabs.retain_workspaces(|workspace| workspace == &ws("b"));
        assert_eq!(tabs.tabs(), &[Tab::Workspace(ws("b")), Tab::New]);
    }

    #[test]
    fn restoring_drops_duplicates() {
        let tabs = Tabs::restore([ws("a"), ws("a")]);
        assert_eq!(tabs.tabs().len(), 1);
        assert_eq!(tabs.active(), None);
    }
}
