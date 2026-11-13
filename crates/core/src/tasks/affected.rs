//! Affected-file checks for task manifest nodes.

use super::{InputPattern, Task, TaskGroup, TaskNode};
use crate::{InputFrame, PatternMatch};
use std::path::PathBuf;

impl crate::AffectedBy for Task {
    /// Returns true if this task is affected by the given file changes.
    ///
    /// # Behavior
    ///
    /// - Tasks with NO inputs are always considered affected (we can't
    ///   determine what affects them). Paths of the project, paths of file
    ///   references and the outputs of other tasks all count.
    /// - Tasks with inputs are affected if any input pattern matches changed
    ///   files, each read from where it starts. The outputs of another task
    ///   change with that task, which is one of this task's dependencies:
    ///   they affect it through that dependency.
    fn is_affected_by(&self, changed_files: &[PathBuf], frame: InputFrame<'_>) -> bool {
        let patterns: Vec<InputPattern<'_>> = self.input_patterns().collect();

        // No inputs = always affected (we can't determine what affects it)
        if patterns.is_empty() {
            return self.iter_task_output_names().next().is_none();
        }

        patterns.into_iter().any(|pattern| {
            crate::match_input(changed_files, frame, pattern) == PatternMatch::Matched
        })
    }

    fn input_patterns(&self) -> Vec<InputPattern<'_>> {
        Self::input_patterns(self).collect()
    }
}

impl crate::AffectedBy for TaskGroup {
    /// A group is affected if ANY of its subtasks are affected.
    fn is_affected_by(&self, changed_files: &[PathBuf], frame: InputFrame<'_>) -> bool {
        self.children
            .values()
            .any(|node| node.is_affected_by(changed_files, frame))
    }

    fn input_patterns(&self) -> Vec<InputPattern<'_>> {
        self.children
            .values()
            .flat_map(crate::AffectedBy::input_patterns)
            .collect()
    }
}

impl crate::AffectedBy for TaskNode {
    fn is_affected_by(&self, changed_files: &[PathBuf], frame: InputFrame<'_>) -> bool {
        match self {
            Self::Task(task) => task.is_affected_by(changed_files, frame),
            Self::Group(group) => group.is_affected_by(changed_files, frame),
            Self::Sequence(seq) => seq
                .iter()
                .any(|node| node.is_affected_by(changed_files, frame)),
        }
    }

    fn input_patterns(&self) -> Vec<InputPattern<'_>> {
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
