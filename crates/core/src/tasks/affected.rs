//! Affected-file checks for task manifest nodes.

use super::{Task, TaskGroup, TaskNode};
use std::path::Path;

impl crate::AffectedBy for Task {
    /// Returns true if this task is affected by the given file changes.
    ///
    /// # Behavior
    ///
    /// - Tasks with NO inputs are always considered affected (we can't determine what affects them)
    /// - Tasks with inputs are affected if any input pattern matches changed files
    fn is_affected_by(&self, changed_files: &[std::path::PathBuf], project_root: &Path) -> bool {
        let inputs: Vec<_> = self.iter_path_inputs().collect();

        // No inputs = always affected (we can't determine what affects it)
        if inputs.is_empty() {
            return true;
        }

        // Check if any input pattern matches any changed file
        inputs
            .iter()
            .any(|pattern| crate::matches_pattern(changed_files, project_root, pattern))
    }

    fn input_patterns(&self) -> Vec<&str> {
        self.iter_path_inputs().collect()
    }
}

impl crate::AffectedBy for TaskGroup {
    /// A group is affected if ANY of its subtasks are affected.
    fn is_affected_by(&self, changed_files: &[std::path::PathBuf], project_root: &Path) -> bool {
        self.children
            .values()
            .any(|node| node.is_affected_by(changed_files, project_root))
    }

    fn input_patterns(&self) -> Vec<&str> {
        self.children
            .values()
            .flat_map(|node| crate::AffectedBy::input_patterns(node))
            .collect()
    }
}

impl crate::AffectedBy for TaskNode {
    fn is_affected_by(&self, changed_files: &[std::path::PathBuf], project_root: &Path) -> bool {
        match self {
            Self::Task(task) => task.is_affected_by(changed_files, project_root),
            Self::Group(group) => group.is_affected_by(changed_files, project_root),
            Self::Sequence(seq) => seq
                .iter()
                .any(|node| node.is_affected_by(changed_files, project_root)),
        }
    }

    fn input_patterns(&self) -> Vec<&str> {
        match self {
            Self::Task(task) => crate::AffectedBy::input_patterns(task.as_ref()),
            Self::Group(group) => crate::AffectedBy::input_patterns(group),
            Self::Sequence(seq) => seq
                .iter()
                .flat_map(crate::AffectedBy::input_patterns)
                .collect(),
        }
    }
}
