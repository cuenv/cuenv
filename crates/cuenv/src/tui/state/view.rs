use std::collections::HashSet;

/// Output display mode for the output panel
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputMode {
    /// Show all task outputs grouped by task
    #[default]
    All,
    /// Show only the selected task's output
    Selected,
}

/// Type of node in the tree view
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeNodeType {
    /// Special "All" node that shows combined output
    All,
    /// Intermediate group node (e.g., "test" in "test.bdd")
    Group(String),
    /// Actual task node (full task name)
    Task(String),
}

/// Represents a single item in the flattened tree view
#[derive(Debug, Clone)]
pub struct TreeViewItem {
    /// Type of this node
    pub node_type: TreeNodeType,
    /// Display name (e.g., "bdd" instead of full "task:cuenv:test.bdd")
    pub display_name: String,
    /// Depth in the tree (for indentation)
    pub depth: usize,
    /// Whether this node is expanded
    pub is_expanded: bool,
    /// Whether this node has children
    pub has_children: bool,
}

impl TreeViewItem {
    /// Get the unique key for this node (for expansion tracking)
    #[must_use]
    pub fn node_key(&self) -> String {
        match &self.node_type {
            TreeNodeType::All => "::all::".to_string(),
            TreeNodeType::Group(path) => format!("::group::{path}"),
            TreeNodeType::Task(name) => name.clone(),
        }
    }
}

/// Input-driven view state.
///
/// Mutated only by `TuiState` input-handler methods. Holds the user's cursor
/// position, expansion, focus, and scroll state that should survive event floods.
#[derive(Debug)]
pub struct UiState {
    pub(super) selected_task: Option<String>,
    pub(super) expanded_nodes: HashSet<String>,
    pub(super) cursor_position: usize,
    pub(super) flattened_tree: Vec<TreeViewItem>,
    pub(super) output_mode: OutputMode,
    pub(super) output_scroll: usize,
    pub(super) focused_task: Option<String>,
}

impl UiState {
    pub(super) fn new() -> Self {
        Self {
            selected_task: None,
            expanded_nodes: HashSet::new(),
            cursor_position: 0,
            flattened_tree: Vec::new(),
            output_mode: OutputMode::All,
            output_scroll: 0,
            focused_task: None,
        }
    }

    /// Task name currently selected for filtered output, if any.
    #[must_use]
    pub fn selected_task(&self) -> Option<&str> {
        self.selected_task.as_deref()
    }

    /// Set of expanded tree nodes (keys produced by [`TreeViewItem::node_key`]).
    #[must_use]
    pub const fn expanded_nodes(&self) -> &HashSet<String> {
        &self.expanded_nodes
    }

    /// Cursor position in the flattened tree.
    #[must_use]
    pub const fn cursor_position(&self) -> usize {
        self.cursor_position
    }

    /// Cached flattened tree view.
    #[must_use]
    pub fn flattened_tree(&self) -> &[TreeViewItem] {
        &self.flattened_tree
    }

    /// Output panel display mode.
    #[must_use]
    pub const fn output_mode(&self) -> OutputMode {
        self.output_mode
    }

    /// Scroll offset for the output panel.
    #[must_use]
    pub const fn output_scroll(&self) -> usize {
        self.output_scroll
    }

    /// Task name currently in focused-output mode, if any.
    #[must_use]
    pub fn focused_task(&self) -> Option<&str> {
        self.focused_task.as_deref()
    }
}
