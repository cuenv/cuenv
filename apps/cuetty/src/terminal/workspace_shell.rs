//! Toolkit-neutral workspace routing. Session ownership stays outside this
//! pure model so hosts can replace the terminal backend without leaking it.

use crate::workspace::{
    Error as WorkspaceError, LayoutRect, MinSizePolicy, PaneId, PaneLayout, TabId, Workspace,
    project_node,
};
use std::fmt;

pub const RIO_CONTENT: &str = "rio-session";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceShortcut {
    NewTab,
    CloseTab,
    SplitHorizontal,
    SplitVertical,
    FocusNext,
    FocusPrevious,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TabDirection {
    Previous,
    Next,
}

/// A tab's place on the number keys, following the browser convention: 1 to 8
/// count from the first tab and 9 is always the last.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TabNumber(u8);

impl TabNumber {
    const LAST: u8 = 9;

    pub fn from_key(key: &str) -> Option<Self> {
        key.parse()
            .ok()
            .filter(|digit| (1..=Self::LAST).contains(digit))
            .map(Self)
    }

    /// The number that reaches the tab at `index` of `count`, which only the
    /// first eight tabs and the last one have.
    pub fn of_position(index: usize, count: usize) -> Option<Self> {
        if index >= count {
            None
        } else if index + 1 == count {
            Some(Self(if count < usize::from(Self::LAST) {
                (index + 1) as u8
            } else {
                Self::LAST
            }))
        } else if index + 1 < usize::from(Self::LAST) {
            Some(Self((index + 1) as u8))
        } else {
            None
        }
    }

    pub fn digit(self) -> u8 {
        self.0
    }

    fn position(self, count: usize) -> Option<usize> {
        if self.0 == Self::LAST {
            count.checked_sub(1)
        } else {
            Some(usize::from(self.0) - 1).filter(|index| *index < count)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellAction {
    Shortcut(WorkspaceShortcut),
    ActivateTab(TabId),
    FocusPane(PaneId),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TabTarget {
    pub tab: TabId,
    pub pane: PaneId,
    pub is_last_tab: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub enum ShellEffect {
    ActivePaneChanged(Option<PaneId>),
}
#[derive(Clone, Debug, PartialEq)]
pub enum ShellError {
    Workspace(WorkspaceError),
    MultiSessionUnavailable,
    LifecycleOwnedByHost,
}
impl From<WorkspaceError> for ShellError {
    fn from(error: WorkspaceError) -> Self {
        Self::Workspace(error)
    }
}
impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace(error) => write!(f, "{error}"),
            Self::MultiSessionUnavailable => write!(f, "split panes are not available yet"),
            Self::LifecycleOwnedByHost => {
                write!(f, "tab lifecycle is owned by the terminal session host")
            }
        }
    }
}
impl std::error::Error for ShellError {}

#[derive(Clone, Debug)]
pub struct WorkspaceShell {
    workspace: Workspace<String>,
}
impl WorkspaceShell {
    pub fn empty() -> Self {
        Self {
            workspace: Workspace::new(),
        }
    }
    pub fn new() -> Result<Self, WorkspaceError> {
        let mut shell = Self::empty();
        shell
            .workspace
            .create_tab("Terminal", RIO_CONTENT.to_owned())?;
        Ok(shell)
    }
    pub fn workspace(&self) -> &Workspace<String> {
        &self.workspace
    }
    pub fn active_pane(&self) -> Option<PaneId> {
        self.workspace.active_pane()
    }
    pub fn active_tab(&self) -> Option<TabId> {
        self.workspace.active_tab().map(|tab| tab.id)
    }
    pub fn tab_in_direction(&self, direction: TabDirection) -> Option<TabId> {
        let tabs = self.workspace.tabs();
        let active = self.active_tab()?;
        let index = tabs.iter().position(|tab| tab.id == active)?;
        let target = match direction {
            TabDirection::Previous => index.checked_sub(1).unwrap_or(tabs.len() - 1),
            TabDirection::Next => (index + 1) % tabs.len(),
        };
        Some(tabs[target].id)
    }
    pub fn tab_at_number(&self, number: TabNumber) -> Option<TabId> {
        let tabs = self.workspace.tabs();
        number.position(tabs.len()).map(|index| tabs[index].id)
    }
    pub fn create_tab(&mut self, title: &str) -> Result<TabTarget, ShellError> {
        let tab = self.workspace.create_tab(title, RIO_CONTENT.to_owned())?;
        self.target_for(tab)
    }
    pub fn preflight_close_active_tab(&self) -> Result<TabTarget, ShellError> {
        self.target_for(self.active_tab().ok_or(WorkspaceError::NoTabs)?)
    }
    pub fn preflight_close_pane(&self, pane: PaneId) -> Result<TabTarget, ShellError> {
        let tab = self
            .workspace
            .tabs()
            .iter()
            .find(|tab| tab.focused == pane)
            .ok_or(WorkspaceError::UnknownPane)?;
        self.target_for(tab.id)
    }
    /// Commit only after the caller removed the matching session entry.
    pub fn commit_close_preflighted(&mut self, target: TabTarget) {
        self.workspace
            .close_tab(target.tab)
            .expect("preflighted tab must close");
    }
    pub fn apply(&mut self, action: ShellAction) -> Result<ShellEffect, ShellError> {
        match action {
            ShellAction::ActivateTab(tab) => self.workspace.activate_tab(tab)?,
            ShellAction::FocusPane(pane) => self.workspace.focus_pane(pane)?,
            ShellAction::Shortcut(
                WorkspaceShortcut::SplitHorizontal | WorkspaceShortcut::SplitVertical,
            ) => return Err(ShellError::MultiSessionUnavailable),
            ShellAction::Shortcut(WorkspaceShortcut::NewTab | WorkspaceShortcut::CloseTab) => {
                return Err(ShellError::LifecycleOwnedByHost);
            }
            ShellAction::Shortcut(WorkspaceShortcut::FocusNext) => {
                self.workspace.focus_next()?;
            }
            ShellAction::Shortcut(WorkspaceShortcut::FocusPrevious) => {
                self.workspace.focus_previous()?;
            }
        }
        Ok(ShellEffect::ActivePaneChanged(self.active_pane()))
    }
    pub fn project(
        &self,
        bounds: LayoutRect,
        minimum: MinSizePolicy,
    ) -> Result<Vec<PaneLayout>, crate::workspace::ProjectionError> {
        match self.workspace.active_tab() {
            Some(tab) => project_node(&tab.root, bounds, Some(tab.focused), minimum),
            None => Ok(Vec::new()),
        }
    }
    fn target_for(&self, tab_id: TabId) -> Result<TabTarget, ShellError> {
        let tab = self
            .workspace
            .tabs()
            .iter()
            .find(|tab| tab.id == tab_id)
            .ok_or(WorkspaceError::UnknownTab)?;
        Ok(TabTarget {
            tab: tab.id,
            pane: tab.focused,
            is_last_tab: self.workspace.tabs().len() == 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_tabs_follow_display_order_and_wrap() {
        let mut shell = WorkspaceShell::empty();
        assert_eq!(shell.tab_in_direction(TabDirection::Next), None);

        let first = shell.create_tab("first").unwrap();
        assert_eq!(shell.tab_in_direction(TabDirection::Next), Some(first.tab));
        assert_eq!(
            shell.tab_in_direction(TabDirection::Previous),
            Some(first.tab)
        );
        let second = shell.create_tab("second").unwrap();
        let third = shell.create_tab("third").unwrap();

        assert_eq!(shell.tab_in_direction(TabDirection::Next), Some(first.tab));
        assert_eq!(
            shell.tab_in_direction(TabDirection::Previous),
            Some(second.tab)
        );
        shell.apply(ShellAction::ActivateTab(first.tab)).unwrap();
        assert_eq!(shell.tab_in_direction(TabDirection::Next), Some(second.tab));
        assert_eq!(
            shell.tab_in_direction(TabDirection::Previous),
            Some(third.tab)
        );
        shell.apply(ShellAction::ActivateTab(second.tab)).unwrap();
        assert_eq!(shell.tab_in_direction(TabDirection::Next), Some(third.tab));
        assert_eq!(
            shell.tab_in_direction(TabDirection::Previous),
            Some(first.tab)
        );
    }

    #[test]
    fn only_the_digits_one_to_nine_are_tab_numbers() {
        for key in ["0", "10", "a", ""] {
            assert_eq!(TabNumber::from_key(key), None);
        }
        assert_eq!(TabNumber::from_key("1").map(TabNumber::digit), Some(1));
        assert_eq!(TabNumber::from_key("9").map(TabNumber::digit), Some(9));
    }

    #[test]
    fn number_keys_count_from_the_first_tab_and_nine_is_the_last() {
        let mut shell = WorkspaceShell::empty();
        let number = |key| TabNumber::from_key(key).unwrap();
        assert_eq!(shell.tab_at_number(number("1")), None);
        assert_eq!(shell.tab_at_number(number("9")), None);
        let first = shell.create_tab("first").unwrap().tab;
        let second = shell.create_tab("second").unwrap().tab;
        let third = shell.create_tab("third").unwrap().tab;
        assert_eq!(shell.tab_at_number(number("1")), Some(first));
        assert_eq!(shell.tab_at_number(number("2")), Some(second));
        assert_eq!(shell.tab_at_number(number("3")), Some(third));
        assert_eq!(shell.tab_at_number(number("4")), None);
        assert_eq!(shell.tab_at_number(number("9")), Some(third));
    }

    #[test]
    fn a_hint_shows_a_number_that_reaches_that_tab() {
        let digits = |count| {
            (0..count)
                .map(|index| TabNumber::of_position(index, count).map(TabNumber::digit))
                .collect::<Vec<_>>()
        };
        assert_eq!(digits(3), [Some(1), Some(2), Some(3)]);
        assert_eq!(digits(9).last(), Some(&Some(9)));
        assert_eq!(digits(9)[7], Some(8));
        // Past nine tabs the middle ones have no number, and the last keeps 9.
        let many = digits(12);
        assert_eq!(many[7], Some(8));
        assert_eq!(many[8], None);
        assert_eq!(many[10], None);
        assert_eq!(many[11], Some(9));
        assert_eq!(TabNumber::of_position(3, 3), None);
    }

    #[test]
    fn every_hinted_number_selects_the_tab_it_is_shown_on() {
        let mut shell = WorkspaceShell::empty();
        for _ in 0..12 {
            shell.create_tab("tab").unwrap();
        }
        let tabs = shell.workspace().tabs().to_vec();
        for (index, tab) in tabs.iter().enumerate() {
            if let Some(number) = TabNumber::of_position(index, tabs.len()) {
                assert_eq!(shell.tab_at_number(number), Some(tab.id));
            }
        }
    }

    #[test]
    fn close_preflight_is_exact_and_last_policy_is_explicit() {
        let mut shell = WorkspaceShell::new().unwrap();
        let first = shell.preflight_close_active_tab().unwrap();
        assert!(first.is_last_tab);
        let second = shell.create_tab("Terminal 2").unwrap();
        assert!(!second.is_last_tab);
        let close = shell.preflight_close_active_tab().unwrap();
        assert_eq!(close, second);
        shell.commit_close_preflighted(close);
        assert_eq!(shell.active_tab(), Some(first.tab));
    }
    #[test]
    fn splits_are_rejected_without_mutation() {
        let mut shell = WorkspaceShell::new().unwrap();
        let before_tabs = shell.workspace().tabs().to_vec();
        let before_active = shell.active_tab();
        assert_eq!(
            shell.apply(ShellAction::Shortcut(WorkspaceShortcut::SplitHorizontal)),
            Err(ShellError::MultiSessionUnavailable)
        );
        assert_eq!(shell.workspace().tabs(), before_tabs);
        assert_eq!(shell.active_tab(), before_active);
    }
}
