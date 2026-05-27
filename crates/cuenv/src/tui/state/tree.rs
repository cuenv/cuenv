use super::{OutputMode, TreeNodeType, TreeViewItem, TuiState};
use std::collections::HashMap;

impl TuiState {
    /// Extract the task path from a full task name.
    /// Task names follow the format: `task:project:path.parts`
    /// Returns the `path.parts` portion split by dots.
    #[must_use]
    pub(super) fn parse_task_path(task_name: &str) -> Vec<&str> {
        let parts: Vec<&str> = task_name.split(':').collect();
        if parts.len() >= 3 {
            parts[2].split('.').collect()
        } else if parts.len() == 2 {
            parts[1].split('.').collect()
        } else {
            vec![task_name]
        }
    }

    /// Build a hierarchical tree structure from task names.
    /// Returns a nested structure: `path -> (child_groups, tasks_at_this_level)`.
    pub(super) fn build_name_hierarchy(&self) -> HashMap<String, (Vec<String>, Vec<String>)> {
        let mut tree: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
        tree.insert(String::new(), (Vec::new(), Vec::new()));

        for task_name in self.model.tasks.keys() {
            let path_parts = Self::parse_task_path(task_name);
            let group_parts = if path_parts.len() > 1 {
                &path_parts[..path_parts.len() - 1]
            } else {
                &[]
            };

            let mut current_path = String::new();
            for part in group_parts {
                let parent_path = current_path.clone();
                if current_path.is_empty() {
                    current_path = (*part).to_string();
                } else {
                    current_path = format!("{current_path}.{part}");
                }

                tree.entry(current_path.clone())
                    .or_insert_with(|| (Vec::new(), Vec::new()));

                let (groups, _) = tree
                    .entry(parent_path)
                    .or_insert_with(|| (Vec::new(), Vec::new()));
                if !groups.contains(&current_path) {
                    groups.push(current_path.clone());
                }
            }

            let parent_path = if path_parts.len() > 1 {
                path_parts[..path_parts.len() - 1].join(".")
            } else {
                String::new()
            };

            let (_, tasks) = tree
                .entry(parent_path)
                .or_insert_with(|| (Vec::new(), Vec::new()));
            if !tasks.contains(task_name) {
                tasks.push(task_name.clone());
            }
        }

        for (groups, tasks) in tree.values_mut() {
            groups.sort();
            tasks.sort();
        }

        tree
    }

    /// Rebuild the flattened tree view based on current expansion state.
    pub fn rebuild_flattened_tree(&mut self) {
        let tree = self.build_name_hierarchy();
        let mut flattened = Vec::new();

        let all_key = "::all::".to_string();
        let all_expanded = self.ui.expanded_nodes.contains(&all_key);
        let has_tasks = !self.model.tasks.is_empty();

        flattened.push(TreeViewItem {
            node_type: TreeNodeType::All,
            display_name: "All".to_string(),
            depth: 0,
            is_expanded: all_expanded,
            has_children: has_tasks,
        });

        if all_expanded && let Some((root_groups, root_tasks)) = tree.get("") {
            self.push_expanded_tree_nodes(&tree, root_groups, root_tasks, &mut flattened);
        }

        self.ui.flattened_tree = flattened;

        if self.ui.cursor_position >= self.ui.flattened_tree.len() {
            self.ui.cursor_position = self.ui.flattened_tree.len().saturating_sub(1);
        }
    }

    fn push_expanded_tree_nodes(
        &self,
        tree: &HashMap<String, (Vec<String>, Vec<String>)>,
        root_groups: &[String],
        root_tasks: &[String],
        flattened: &mut Vec<TreeViewItem>,
    ) {
        let mut stack: Vec<TreeStackItem> = Vec::new();

        for task_name in root_tasks.iter().rev() {
            stack.push(TreeStackItem::task(task_name, 1));
        }

        for group_path in root_groups.iter().rev() {
            stack.push(TreeStackItem::group(group_path, group_path, 1));
        }

        while let Some(item) = stack.pop() {
            if item.is_group {
                self.push_group_node(tree, item, flattened, &mut stack);
            } else {
                flattened.push(TreeViewItem {
                    node_type: TreeNodeType::Task(item.path),
                    display_name: item.display_name,
                    depth: item.depth,
                    is_expanded: false,
                    has_children: false,
                });
            }
        }
    }

    fn push_group_node(
        &self,
        tree: &HashMap<String, (Vec<String>, Vec<String>)>,
        item: TreeStackItem,
        flattened: &mut Vec<TreeViewItem>,
        stack: &mut Vec<TreeStackItem>,
    ) {
        let group_key = format!("::group::{}", item.path);
        let is_expanded = self.ui.expanded_nodes.contains(&group_key);
        let empty_vec: Vec<String> = Vec::new();
        let (child_groups, child_tasks) = tree
            .get(&item.path)
            .map_or((&empty_vec, &empty_vec), |(g, t)| (g, t));
        let has_children = !child_groups.is_empty() || !child_tasks.is_empty();

        flattened.push(TreeViewItem {
            node_type: TreeNodeType::Group(item.path.clone()),
            display_name: item.display_name,
            depth: item.depth,
            is_expanded,
            has_children,
        });

        if is_expanded {
            for task_name in child_tasks.iter().rev() {
                stack.push(TreeStackItem::task(task_name, item.depth + 1));
            }
            for child_path in child_groups.iter().rev() {
                let child_display = child_path
                    .split('.')
                    .next_back()
                    .unwrap_or(child_path)
                    .to_string();
                stack.push(TreeStackItem::group(
                    child_path,
                    &child_display,
                    item.depth + 1,
                ));
            }
        }
    }

    /// Toggle expansion state of a tree node.
    pub fn toggle_expansion(&mut self, node_key: &str) {
        if self.ui.expanded_nodes.contains(node_key) {
            self.ui.expanded_nodes.remove(node_key);
        } else {
            self.ui.expanded_nodes.insert(node_key.to_string());
        }
        self.rebuild_flattened_tree();
    }

    /// Move cursor up in tree.
    pub const fn cursor_up(&mut self) {
        if self.ui.cursor_position > 0 {
            self.ui.cursor_position -= 1;
        }
    }

    /// Move cursor down in tree.
    pub const fn cursor_down(&mut self) {
        if self.ui.cursor_position < self.ui.flattened_tree.len().saturating_sub(1) {
            self.ui.cursor_position += 1;
        }
    }

    /// Get currently highlighted node.
    #[must_use]
    pub fn highlighted_node(&self) -> Option<&TreeViewItem> {
        self.ui.flattened_tree.get(self.ui.cursor_position)
    }

    /// Select current node for output filtering.
    pub fn select_current_node(&mut self) {
        if let Some(node) = self.highlighted_node() {
            match &node.node_type {
                TreeNodeType::All => {
                    self.ui.selected_task = None;
                    self.ui.output_mode = OutputMode::All;
                }
                TreeNodeType::Task(name) => {
                    self.ui.selected_task = Some(name.clone());
                    self.ui.output_mode = OutputMode::Selected;
                }
                TreeNodeType::Group(path) => {
                    self.ui.selected_task = Some(format!("::group::{path}"));
                    self.ui.output_mode = OutputMode::Selected;
                }
            }
            self.ui.output_scroll = 0;
        }
    }

    /// Return to "All" output mode.
    pub fn show_all_output(&mut self) {
        self.ui.selected_task = None;
        self.ui.output_mode = OutputMode::All;
        self.ui.output_scroll = 0;
    }

    /// Initialize tree with "All" expanded by default.
    pub fn init_tree(&mut self) {
        self.ui.expanded_nodes.insert("::all::".to_string());
        self.rebuild_flattened_tree();
    }
}

struct TreeStackItem {
    path: String,
    display_name: String,
    depth: usize,
    is_group: bool,
}

impl TreeStackItem {
    fn task(task_name: &str, depth: usize) -> Self {
        let path_parts = TuiState::parse_task_path(task_name);
        let display_name = path_parts
            .last()
            .map_or(task_name.to_string(), |s| (*s).to_string());
        Self {
            path: task_name.to_string(),
            display_name,
            depth,
            is_group: false,
        }
    }

    fn group(path: &str, display_name: &str, depth: usize) -> Self {
        Self {
            path: path.to_string(),
            display_name: display_name.to_string(),
            depth,
            is_group: true,
        }
    }
}
