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
