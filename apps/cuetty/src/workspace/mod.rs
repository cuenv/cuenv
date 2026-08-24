//! Pure workspace layout model.
//!
//! This module intentionally has no GPUI, Rio, or terminal dependencies.  It is
//! the small state machine that a view can project onto those systems.

mod projection;
mod store;

pub use projection::{LayoutRect, MinSizePolicy, PaneLayout, ProjectionError, project_node};
pub use store::{
    BinarySnapshotCodec, CURRENT_SCHEMA_VERSION, FileSnapshotStorage, InMemoryWorkspaceStore,
    PaneMetadata, SessionMetadata, SnapshotCodec, SnapshotStorage, StoreError,
    WorkspacePersistence, WorkspaceSnapshot, WorkspaceStore,
};

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PaneId(u64);
impl PaneId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TabId(u64);
impl TabId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SplitId(u64);
impl SplitId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// A pane content boundary.  The layout stores only this stable identity; a
/// GPUI host can use its own factory to turn it into an entity or view.
pub trait PaneContent: Clone {
    fn identity(&self) -> &str;
}
impl PaneContent for String {
    fn identity(&self) -> &str {
        self
    }
}
impl PaneContent for &'static str {
    fn identity(&self) -> &str {
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pane<C = String> {
    pub id: PaneId,
    pub content: C,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node<C = String> {
    Pane(Pane<C>),
    Split {
        id: SplitId,
        axis: Axis,
        weights: Vec<f32>,
        children: Vec<Node<C>>,
    },
}

impl<C> Node<C> {
    fn pane_ids(&self, out: &mut Vec<PaneId>) {
        match self {
            Self::Pane(p) => out.push(p.id),
            Self::Split { children, .. } => children.iter().for_each(|c| c.pane_ids(out)),
        }
    }
    fn pane_count(&self) -> usize {
        let mut p = Vec::new();
        self.pane_ids(&mut p);
        p.len()
    }
    fn contains(&self, id: PaneId) -> bool {
        match self {
            Self::Pane(p) => p.id == id,
            Self::Split { children, .. } => children.iter().any(|c| c.contains(id)),
        }
    }
    fn find_split_mut(&mut self, id: SplitId) -> Option<SplitParts<'_, C>> {
        match self {
            Self::Pane(_) => None,
            Self::Split {
                id: own,
                weights,
                children,
                ..
            } => {
                if *own == id {
                    Some((weights, children))
                } else {
                    children.iter_mut().find_map(|c| c.find_split_mut(id))
                }
            }
        }
    }
    fn normalize(&mut self) {
        if let Self::Split {
            weights, children, ..
        } = self
        {
            weights.resize(children.len(), 1.0);
            let sum: f32 = weights.iter().copied().sum();
            if sum <= f32::EPSILON {
                weights.fill(1.0);
            }
            let sum: f32 = weights.iter().copied().sum();
            weights.iter_mut().for_each(|w| *w = (*w / sum).max(0.0));
            let sum: f32 = weights.iter().copied().sum();
            weights.iter_mut().for_each(|w| *w /= sum);
            children.iter_mut().for_each(Self::normalize);
        }
    }
    fn collapse(self) -> Self {
        match self {
            Self::Split { mut children, .. } if children.len() == 1 => {
                children.remove(0).collapse()
            }
            Self::Split {
                id,
                axis,
                weights,
                children,
            } => Self::Split {
                id,
                axis,
                weights,
                children: children.into_iter().map(Self::collapse).collect(),
            },
            x => x,
        }
    }
    fn validate(&self, panes: &mut Vec<PaneId>, splits: &mut Vec<SplitId>) -> bool {
        match self {
            Self::Pane(p) => {
                if panes.contains(&p.id) {
                    false
                } else {
                    panes.push(p.id);
                    true
                }
            }
            Self::Split {
                id,
                weights,
                children,
                ..
            } => {
                !splits.contains(id)
                    && {
                        splits.push(*id);
                        true
                    }
                    && weights.len() == children.len()
                    && !children.is_empty()
                    && (weights.iter().sum::<f32>() - 1.0).abs() < 0.001
                    && weights.iter().all(|w| *w > 0.0)
                    && children.iter().all(|c| c.validate(panes, splits))
            }
        }
    }
}

type SplitParts<'a, C> = (&'a mut Vec<f32>, &'a mut Vec<Node<C>>);

pub(crate) fn validate_tab<C>(tab: &Tab<C>) -> bool {
    let mut panes = Vec::new();
    let mut splits = Vec::new();
    tab.root.contains(tab.focused) && tab.root.validate(&mut panes, &mut splits)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tab<C = String> {
    pub id: TabId,
    pub title: String,
    pub root: Node<C>,
    pub focused: PaneId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SavedTree<C = String> {
    pub tab: Tab<C>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command<C = String> {
    CreateTab {
        title: String,
        content: C,
    },
    Split {
        pane: PaneId,
        axis: Axis,
        content: C,
    },
    FocusNext,
    FocusPrevious,
    Resize {
        split: SplitId,
        boundary: usize,
        delta: f32,
    },
    CloseActive,
    Reopen,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CommandResult {
    TabCreated(TabId),
    PaneCreated(PaneId),
    Focused(PaneId),
    Resized,
    Closed(PaneId),
    Reopened(PaneId),
    Noop,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    IdExhausted,
    NoTabs,
    UnknownTab,
    UnknownPane,
    UnknownSplit,
    InvalidBoundary,
    CannotCloseLastPane,
    NothingToReopen,
    InvalidTree,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Debug)]
pub struct Workspace<C = String> {
    tabs: Vec<Tab<C>>,
    active: Option<TabId>,
    // `None` records that `u64::MAX` was already allocated.  Keeping this
    // distinct from the last ID prevents restores from turning exhaustion into
    // an accidental collision with the maximum value.
    next_pane: Option<u64>,
    next_tab: Option<u64>,
    next_split: Option<u64>,
    pub min_pane_fraction: f32,
    closed: Vec<SavedTree<C>>,
}

impl<C: Clone> Default for Workspace<C> {
    fn default() -> Self {
        Self::new()
    }
}
impl<C: Clone> Workspace<C> {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            next_pane: Some(1),
            next_tab: Some(1),
            next_split: Some(1),
            min_pane_fraction: 0.05,
            closed: Vec::new(),
        }
    }
    pub fn tabs(&self) -> &[Tab<C>] {
        &self.tabs
    }
    pub fn active_tab(&self) -> Option<&Tab<C>> {
        self.active
            .and_then(|id| self.tabs.iter().find(|t| t.id == id))
    }
    pub fn active_pane(&self) -> Option<PaneId> {
        self.active_tab().map(|t| t.focused)
    }
    /// Makes an existing tab active without changing its layout or focus.
    pub fn activate_tab(&mut self, tab_id: TabId) -> Result<(), Error> {
        if self.tabs.iter().any(|tab| tab.id == tab_id) {
            self.active = Some(tab_id);
            Ok(())
        } else {
            Err(Error::UnknownTab)
        }
    }
    /// Focuses an existing pane in the active tab.  A pane in another tab is
    /// intentionally not selected implicitly: callers must choose the tab
    /// first so tab changes remain explicit at the UI boundary.
    pub fn focus_pane(&mut self, pane_id: PaneId) -> Result<(), Error> {
        let tab = self.active_mut()?;
        if tab.root.contains(pane_id) {
            tab.focused = pane_id;
            Ok(())
        } else {
            Err(Error::UnknownPane)
        }
    }
    pub fn create_tab(&mut self, title: impl Into<String>, content: C) -> Result<TabId, Error> {
        let tab_id = self.peek_id(self.next_tab)?;
        let pane_id = self.peek_id(self.next_pane)?;
        let tab = Tab {
            id: TabId(tab_id),
            title: title.into(),
            root: Node::Pane(Pane {
                id: PaneId(pane_id),
                content,
            }),
            focused: PaneId(pane_id),
        };
        self.advance_tab();
        self.advance_pane();
        let id = tab.id;
        self.tabs.push(tab);
        self.active = Some(id);
        Ok(id)
    }
    fn active_mut(&mut self) -> Result<&mut Tab<C>, Error> {
        let id = self.active.ok_or(Error::NoTabs)?;
        self.tabs
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(Error::UnknownTab)
    }
    pub fn split(&mut self, pane: PaneId, axis: Axis, content: C) -> Result<PaneId, Error> {
        // Validate the target before allocating IDs.  A failed command must be
        // indistinguishable from a command that was never attempted.
        let active = self.active.ok_or(Error::NoTabs)?;
        let tab_index = self
            .tabs
            .iter()
            .position(|tab| tab.id == active)
            .ok_or(Error::UnknownTab)?;
        if !self.tabs[tab_index].root.contains(pane) {
            return Err(Error::UnknownPane);
        }

        // Build every mutable part of the result independently, then commit
        // the tree, focus, and allocators together only after construction
        // succeeds.  In particular, exhaustion cannot partially consume an ID.
        let split_id = SplitId(self.peek_id(self.next_split)?);
        let pane_id = PaneId(self.peek_id(self.next_pane)?);
        let new = Pane {
            id: pane_id,
            content,
        };
        let next_split = self.next_split.and_then(|id| id.checked_add(1));
        let next_pane = self.next_pane.and_then(|id| id.checked_add(1));
        let (root, did_split) =
            split_node(self.tabs[tab_index].root.clone(), pane, split_id, axis, new);
        debug_assert!(did_split, "target was validated in the active tab");
        if !did_split {
            return Err(Error::UnknownPane);
        }

        self.tabs[tab_index].root = root;
        self.tabs[tab_index].focused = pane_id;
        self.next_split = next_split;
        self.next_pane = next_pane;
        Ok(pane_id)
    }
    pub fn focus_next(&mut self) -> Result<PaneId, Error> {
        self.focus_by(1)
    }
    pub fn focus_previous(&mut self) -> Result<PaneId, Error> {
        self.focus_by(-1)
    }
    fn focus_by(&mut self, step: isize) -> Result<PaneId, Error> {
        let tab = self.active_mut()?;
        let mut ids = Vec::new();
        tab.root.pane_ids(&mut ids);
        let pos = ids
            .iter()
            .position(|x| *x == tab.focused)
            .ok_or(Error::InvalidTree)?;
        let next = ids[(pos as isize + step).rem_euclid(ids.len() as isize) as usize];
        tab.focused = next;
        Ok(next)
    }
    pub fn resize(&mut self, split: SplitId, boundary: usize, delta: f32) -> Result<(), Error> {
        let min = self.min_pane_fraction.max(0.0);
        let tab = self.active_mut()?;
        let (weights, children) = tab.root.find_split_mut(split).ok_or(Error::UnknownSplit)?;
        if boundary + 1 >= weights.len() {
            return Err(Error::InvalidBoundary);
        }
        let left_min = min * children[boundary].pane_count() as f32;
        let right_min = min * children[boundary + 1].pane_count() as f32;
        let available = weights[boundary] + weights[boundary + 1];
        let lo = left_min.min(available - right_min);
        let hi = (available - right_min).max(lo).max(left_min);
        let left = (weights[boundary] + delta).clamp(lo, hi);
        weights[boundary] = left;
        weights[boundary + 1] = available - left;
        tab.root.normalize();
        Ok(())
    }
    pub fn close_active(&mut self) -> Result<PaneId, Error> {
        let (closed, id) = {
            let tab = self.active_mut()?;
            if tab.root.pane_count() == 1 {
                return Err(Error::CannotCloseLastPane);
            }
            let closed = SavedTree { tab: tab.clone() };
            let id = tab.focused;
            let mut ids = Vec::new();
            tab.root.pane_ids(&mut ids);
            let replacement = ids
                .iter()
                .copied()
                .find(|p| *p != id)
                .ok_or(Error::InvalidTree)?;
            remove_pane(&mut tab.root, id);
            tab.root = tab.root.clone().collapse();
            tab.focused = replacement;
            tab.root.normalize();
            (closed, id)
        };
        self.closed.push(closed);
        Ok(id)
    }
    /// Removes the active tab as an atomic shell-level operation.  This is
    /// distinct from `close_active`, which removes only the focused pane and
    /// keeps at least one pane alive in its tab.
    pub fn close_active_tab(&mut self) -> Result<TabId, Error> {
        let active = self.active.ok_or(Error::NoTabs)?;
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.id == active)
            .ok_or(Error::UnknownTab)?;
        self.tabs.remove(index);
        self.active = self
            .tabs
            .get(index)
            .or_else(|| self.tabs.last())
            .map(|tab| tab.id);
        Ok(active)
    }
    /// Removes an exact tab without changing active selection first.
    pub fn close_tab(&mut self, tab_id: TabId) -> Result<TabId, Error> {
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.id == tab_id)
            .ok_or(Error::UnknownTab)?;
        self.tabs.remove(index);
        if self.active == Some(tab_id) {
            self.active = self
                .tabs
                .get(index)
                .or_else(|| self.tabs.last())
                .map(|tab| tab.id);
        }
        Ok(tab_id)
    }
    pub fn reopen(&mut self) -> Result<PaneId, Error> {
        let saved = self.closed.last().ok_or(Error::NothingToReopen)?;
        let id = saved.tab.focused;
        let tab = self
            .tabs
            .iter_mut()
            .find(|tab| tab.id == saved.tab.id)
            .ok_or(Error::UnknownTab)?;
        *tab = saved.tab.clone();
        self.closed.pop();
        Ok(id)
    }
    pub fn apply(&mut self, command: Command<C>) -> Result<CommandResult, Error> {
        Ok(match command {
            Command::CreateTab { title, content } => {
                CommandResult::TabCreated(self.create_tab(title, content)?)
            }
            Command::Split {
                pane,
                axis,
                content,
            } => CommandResult::PaneCreated(self.split(pane, axis, content)?),
            Command::FocusNext => CommandResult::Focused(self.focus_next()?),
            Command::FocusPrevious => CommandResult::Focused(self.focus_previous()?),
            Command::Resize {
                split,
                boundary,
                delta,
            } => {
                self.resize(split, boundary, delta)?;
                CommandResult::Resized
            }
            Command::CloseActive => CommandResult::Closed(self.close_active()?),
            Command::Reopen => CommandResult::Reopened(self.reopen()?),
        })
    }
    pub fn validate(&self) -> bool {
        let Some(active) = self.active else {
            return self.tabs.is_empty();
        };
        if !self.tabs.iter().any(|tab| tab.id == active) {
            return false;
        }

        let mut tabs = Vec::new();
        let mut panes = Vec::new();
        let mut splits = Vec::new();
        self.tabs.iter().all(|tab| {
            if tabs.contains(&tab.id) || !tab.root.contains(tab.focused) {
                return false;
            }
            tabs.push(tab.id);
            tab.root.validate(&mut panes, &mut splits)
        })
    }
    pub fn save_tree(&self) -> Result<SavedTree<C>, Error> {
        Ok(SavedTree {
            tab: self.active_tab().ok_or(Error::NoTabs)?.clone(),
        })
    }
    pub fn restore_tree(&mut self, saved: SavedTree<C>) -> Result<(), Error> {
        if !validate_tab(&saved.tab) {
            return Err(Error::InvalidTree);
        }
        let saved_id = saved.tab.id;

        let mut tabs = self.tabs.clone();
        if let Some(index) = tabs.iter().position(|tab| tab.id == saved_id) {
            tabs[index] = saved.tab;
        } else {
            tabs.push(saved.tab);
        }
        let mut max_pane = 0;
        let mut max_split = 0;
        let mut max_tab = 0;
        for tab in &tabs {
            max_tab = max_tab.max(tab.id.get());
            max_layout_ids(&tab.root, &mut max_pane, &mut max_split);
        }
        let candidate = Self {
            tabs,
            active: Some(saved_id),
            next_pane: allocator_after_restore(self.next_pane, max_pane),
            next_tab: allocator_after_restore(self.next_tab, max_tab),
            next_split: allocator_after_restore(self.next_split, max_split),
            min_pane_fraction: self.min_pane_fraction,
            closed: self.closed.clone(),
        };
        if !candidate.validate() {
            return Err(Error::InvalidTree);
        }
        self.tabs = candidate.tabs;
        self.active = candidate.active;
        self.next_pane = candidate.next_pane;
        self.next_tab = candidate.next_tab;
        self.next_split = candidate.next_split;
        Ok(())
    }
}

impl<C: Clone> Workspace<C> {
    fn peek_id(&self, next: Option<u64>) -> Result<u64, Error> {
        next.ok_or(Error::IdExhausted)
    }

    fn advance_pane(&mut self) {
        self.next_pane = self.next_pane.and_then(|id| id.checked_add(1));
    }

    fn advance_tab(&mut self) {
        self.next_tab = self.next_tab.and_then(|id| id.checked_add(1));
    }
}

fn allocator_after_restore(current: Option<u64>, restored_max: u64) -> Option<u64> {
    current.and_then(|next| {
        restored_max
            .checked_add(1)
            .map(|restored_next| next.max(restored_next))
    })
}

fn max_layout_ids<C>(node: &Node<C>, max_pane: &mut u64, max_split: &mut u64) {
    match node {
        Node::Pane(pane) => *max_pane = (*max_pane).max(pane.id.get()),
        Node::Split { id, children, .. } => {
            *max_split = (*max_split).max(id.get());
            for child in children {
                max_layout_ids(child, max_pane, max_split);
            }
        }
    }
}

fn split_node<C: Clone>(
    node: Node<C>,
    pane: PaneId,
    split: SplitId,
    axis: Axis,
    new: Pane<C>,
) -> (Node<C>, bool) {
    match node {
        Node::Pane(old) if old.id == pane => (
            Node::Split {
                id: split,
                axis,
                weights: vec![0.5, 0.5],
                children: vec![Node::Pane(old), Node::Pane(new)],
            },
            true,
        ),
        Node::Pane(old) => (Node::Pane(old), false),
        Node::Split {
            id,
            axis: own_axis,
            weights,
            children,
        } => {
            let mut found = false;
            let children = children
                .into_iter()
                .map(|child| {
                    if found {
                        child
                    } else {
                        let (replacement, did_split) =
                            split_node(child, pane, split, axis, new.clone());
                        found = did_split;
                        replacement
                    }
                })
                .collect();
            (
                Node::Split {
                    id,
                    axis: own_axis,
                    weights,
                    children,
                },
                found,
            )
        }
    }
}
fn remove_pane<C>(node: &mut Node<C>, id: PaneId) -> bool {
    match node {
        Node::Pane(p) => p.id == id,
        Node::Split {
            children, weights, ..
        } => {
            if let Some(i) = children.iter().position(|c| c.contains(id)) {
                children.remove(i);
                weights.remove(i);
                true
            } else {
                children.iter_mut().any(|c| remove_pane(c, id))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn create_and_split() {
        let mut w = Workspace::new();
        let _ = w.create_tab("one", "a").unwrap();
        let first = w.active_pane().unwrap();
        let second = w.split(first, Axis::Horizontal, "b").unwrap();
        assert_eq!(w.active_pane(), Some(second));
        assert!(w.validate());
    }
    #[test]
    fn failed_unknown_pane_split_is_atomic() {
        let mut w = Workspace::new();
        w.create_tab("one", "a").unwrap();
        let first = w.active_pane().unwrap();
        let second = w.split(first, Axis::Horizontal, "b").unwrap();
        w.split(second, Axis::Vertical, "c").unwrap();

        let tabs = w.tabs.clone();
        let active = w.active;
        let next_pane = w.next_pane;
        let next_tab = w.next_tab;
        let next_split = w.next_split;
        let focused = w.active_pane();

        assert_eq!(
            w.split(PaneId::new(99), Axis::Horizontal, "ignored"),
            Err(Error::UnknownPane)
        );
        assert_eq!(w.tabs, tabs);
        assert_eq!(w.active, active);
        assert_eq!(w.active_pane(), focused);
        assert_eq!(w.next_pane, next_pane);
        assert_eq!(w.next_tab, next_tab);
        assert_eq!(w.next_split, next_split);
        assert!(w.validate());

        assert_eq!(w.split(first, Axis::Horizontal, "d"), Ok(PaneId::new(4)));
        let Node::Split { id, .. } = &w.active_tab().unwrap().root else {
            panic!("the root should remain a split");
        };
        assert_eq!(*id, SplitId::new(1));
    }
    #[test]
    fn failed_no_tabs_split_does_not_consume_allocators() {
        let mut w = Workspace::new();
        let next_pane = w.next_pane;
        let next_tab = w.next_tab;
        let next_split = w.next_split;

        assert_eq!(
            w.split(PaneId::new(1), Axis::Horizontal, "ignored"),
            Err(Error::NoTabs)
        );
        assert_eq!(w.next_pane, next_pane);
        assert_eq!(w.next_tab, next_tab);
        assert_eq!(w.next_split, next_split);
        assert!(w.validate());

        assert_eq!(w.create_tab("one", "a"), Ok(TabId::new(1)));
        assert_eq!(w.active_pane(), Some(PaneId::new(1)));
        assert_eq!(
            w.split(PaneId::new(1), Axis::Horizontal, "b"),
            Ok(PaneId::new(2))
        );
        let Node::Split { id, .. } = &w.active_tab().unwrap().root else {
            panic!("the pane should have been split");
        };
        assert_eq!(*id, SplitId::new(1));
    }
    #[test]
    fn focus_wraps() {
        let mut w = Workspace::new();
        w.create_tab("x", "a").unwrap();
        let a = w.active_pane().unwrap();
        let b = w.split(a, Axis::Vertical, "b").unwrap();
        assert_eq!(w.focus_next().unwrap(), a);
        assert_eq!(w.focus_previous().unwrap(), b);
    }
    #[test]
    fn resize_obeys_minimum() {
        let mut w = Workspace::new();
        w.create_tab("x", "a").unwrap();
        let a = w.active_pane().unwrap();
        w.split(a, Axis::Horizontal, "b").unwrap();
        let id = match &w.active_tab().unwrap().root {
            Node::Split { id, .. } => *id,
            _ => unreachable!(),
        };
        w.resize(id, 0, -10.0).unwrap();
        if let Node::Split { weights, .. } = &w.active_tab().unwrap().root {
            assert!((weights[0] - w.min_pane_fraction).abs() < 0.001);
        }
    }
    #[test]
    fn close_and_reopen_saved_tree() {
        let mut w = Workspace::new();
        w.create_tab("x", "a").unwrap();
        let a = w.active_pane().unwrap();
        let b = w.split(a, Axis::Horizontal, "b").unwrap();
        assert_eq!(w.close_active().unwrap(), b);
        assert_eq!(w.active_tab().unwrap().root.pane_count(), 1);
        assert_eq!(w.reopen().unwrap(), b);
        assert_eq!(w.active_pane(), Some(b));
        assert!(w.validate());
    }
    #[test]
    fn reopen_restores_the_tab_that_was_closed_without_changing_the_active_tab() {
        let mut w = Workspace::new();
        let a = w.create_tab("a", "one").unwrap();
        let first = w.active_pane().unwrap();
        let closed = w.split(first, Axis::Horizontal, "two").unwrap();
        w.close_active().unwrap();
        let b = w.create_tab("b", "three").unwrap();

        assert_eq!(w.reopen().unwrap(), closed);
        assert_eq!(w.active_tab().map(|tab| tab.id), Some(b));
        assert_eq!(
            w.tabs().iter().find(|tab| tab.id == a).unwrap().focused,
            closed
        );
        assert!(w.validate());
    }
    #[test]
    fn restore_tree_with_a_different_tab_id_selects_the_restored_tab() {
        let mut w = Workspace::new();
        w.create_tab("existing", "one").unwrap();
        let saved = SavedTree {
            tab: Tab {
                id: TabId::new(99),
                title: "restored".into(),
                root: Node::Pane(Pane {
                    id: PaneId::new(100),
                    content: "saved",
                }),
                focused: PaneId::new(100),
            },
        };

        w.restore_tree(saved).unwrap();
        assert_eq!(w.active_tab().map(|tab| tab.id), Some(TabId::new(99)));
        assert!(w.validate());
    }
    #[test]
    fn restore_tree_advances_all_id_allocators() {
        let mut w = Workspace::new();
        let saved = SavedTree {
            tab: Tab {
                id: TabId::new(99),
                title: "restored".into(),
                root: Node::Split {
                    id: SplitId::new(101),
                    axis: Axis::Horizontal,
                    weights: vec![0.5, 0.5],
                    children: vec![
                        Node::Pane(Pane {
                            id: PaneId::new(100),
                            content: "saved",
                        }),
                        Node::Pane(Pane {
                            id: PaneId::new(101),
                            content: "saved",
                        }),
                    ],
                },
                focused: PaneId::new(100),
            },
        };

        w.restore_tree(saved).unwrap();
        let tab = w.create_tab("new", "new").unwrap();
        let pane = w.active_pane().unwrap();
        let new_pane = w.split(pane, Axis::Vertical, "split").unwrap();

        assert_eq!(tab, TabId::new(100));
        assert_eq!(pane, PaneId::new(102));
        assert_eq!(new_pane, PaneId::new(103));
        let Node::Split { id, .. } = &w.active_tab().unwrap().root else {
            panic!("new tab should have been split");
        };
        assert_eq!(*id, SplitId::new(102));
        assert!(w.validate());
    }
    #[test]
    fn restoring_maximum_pane_id_exhausts_tab_creation_without_invalidating_workspace() {
        let mut w = Workspace::new();
        w.restore_tree(SavedTree {
            tab: Tab {
                id: TabId::new(1),
                title: "restored".into(),
                root: Node::Pane(Pane {
                    id: PaneId::new(u64::MAX),
                    content: "saved",
                }),
                focused: PaneId::new(u64::MAX),
            },
        })
        .unwrap();

        assert_eq!(w.create_tab("new", "new"), Err(Error::IdExhausted));
        assert!(w.validate());
    }
    #[test]
    fn restoring_maximum_tab_id_exhausts_tab_creation_without_invalidating_workspace() {
        let mut w = Workspace::new();
        w.restore_tree(SavedTree {
            tab: Tab {
                id: TabId::new(u64::MAX),
                title: "restored".into(),
                root: Node::Pane(Pane {
                    id: PaneId::new(1),
                    content: "saved",
                }),
                focused: PaneId::new(1),
            },
        })
        .unwrap();

        assert_eq!(w.create_tab("new", "new"), Err(Error::IdExhausted));
        assert!(w.validate());
    }
    #[test]
    fn restoring_maximum_split_id_exhausts_split_without_invalidating_workspace() {
        let mut w = Workspace::new();
        let first = PaneId::new(1);
        w.restore_tree(SavedTree {
            tab: Tab {
                id: TabId::new(1),
                title: "restored".into(),
                root: Node::Split {
                    id: SplitId::new(u64::MAX),
                    axis: Axis::Horizontal,
                    weights: vec![0.5, 0.5],
                    children: vec![
                        Node::Pane(Pane {
                            id: first,
                            content: "saved",
                        }),
                        Node::Pane(Pane {
                            id: PaneId::new(2),
                            content: "saved",
                        }),
                    ],
                },
                focused: first,
            },
        })
        .unwrap();

        assert_eq!(
            w.split(first, Axis::Vertical, "new"),
            Err(Error::IdExhausted)
        );
        assert!(w.validate());
    }
    #[test]
    fn validation_rejects_duplicate_tab_split_and_pane_ids_and_invalid_focus() {
        let pane = PaneId::new(1);
        let split = SplitId::new(1);
        let valid_tab = |id| Tab {
            id,
            title: "x".into(),
            root: Node::Split {
                id: split,
                axis: Axis::Horizontal,
                weights: vec![0.5, 0.5],
                children: vec![
                    Node::Pane(Pane {
                        id: pane,
                        content: "a",
                    }),
                    Node::Pane(Pane {
                        id: PaneId::new(2),
                        content: "b",
                    }),
                ],
            },
            focused: pane,
        };
        let mut w = Workspace::new();
        w.tabs = vec![valid_tab(TabId::new(1)), valid_tab(TabId::new(1))];
        w.active = Some(TabId::new(1));
        assert!(!w.validate());

        w.tabs = vec![valid_tab(TabId::new(1))];
        if let Node::Split { children, .. } = &mut w.tabs[0].root {
            children[1] = Node::Pane(Pane {
                id: pane,
                content: "b",
            });
        }
        assert!(!w.validate());

        w.tabs = vec![
            valid_tab(TabId::new(1)),
            Tab {
                id: TabId::new(2),
                title: "y".into(),
                root: Node::Split {
                    id: split,
                    axis: Axis::Horizontal,
                    weights: vec![0.5, 0.5],
                    children: vec![
                        Node::Pane(Pane {
                            id: PaneId::new(3),
                            content: "c",
                        }),
                        Node::Pane(Pane {
                            id: PaneId::new(4),
                            content: "d",
                        }),
                    ],
                },
                focused: PaneId::new(3),
            },
        ];
        assert!(!w.validate());

        w.tabs = vec![valid_tab(TabId::new(1))];
        w.tabs[0].focused = PaneId::new(99);
        assert!(!w.validate());
    }
    #[test]
    fn deterministic_commands() {
        let mut a = Workspace::new();
        let mut b = Workspace::new();
        for w in [&mut a, &mut b] {
            w.apply(Command::CreateTab {
                title: "x".into(),
                content: "a",
            })
            .unwrap();
        }
        assert_eq!(a.tabs().len(), b.tabs().len());
        assert_eq!(a.active_pane(), b.active_pane());
    }
}
