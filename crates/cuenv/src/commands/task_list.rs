//! Task list data structures and formatters.
//!
//! This module provides a clean separation between task list data and its presentation.
//! The `TaskListData` structure captures all information about available tasks,
//! while `TaskListFormatter` implementations handle different output formats.

mod formatters;

pub use formatters::{
    DashboardFormatter, EmojiFormatter, RichFormatter, TablesFormatter, TextFormatter,
};

#[cfg(test)]
pub(super) use formatters::{format_category_name, get_category_emoji, infer_category_from_name};

mod model;

pub use model::{
    TaskListData, TaskListFormatter, TaskListStats, TaskNode, TaskSourceGroup, build_task_list,
};
