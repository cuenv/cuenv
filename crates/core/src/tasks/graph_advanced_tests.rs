//! Advanced DAG builder tests focusing on cross-project references, hooks, and synthetic tasks.
//!
//! These tests extend the basic graph.rs tests to cover more complex scenarios:
//! - Cross-project task references (TaskRef, ProjectReference)
//! - Synthetic task generation from workspace hooks
//! - HookItem variants (TaskRef, MatchHook, inline Task)
//! - Complex dependency chains across projects
//! - Task discovery and matcher integration

use super::*;
use crate::tasks::{TaskDependency, TaskNode};
use crate::test_utils::{create_task, create_task_ref, create_task_with_project_ref};

#[path = "graph_advanced_tests/complex_hooks.rs"]
mod complex_hooks;
#[path = "graph_advanced_tests/cross_project.rs"]
mod cross_project;
#[path = "graph_advanced_tests/cross_project_hooks.rs"]
mod cross_project_hooks;
#[path = "graph_advanced_tests/errors_labels_build.rs"]
mod errors_labels_build;
#[path = "graph_advanced_tests/scale_edge_cases.rs"]
mod scale_edge_cases;
#[path = "graph_advanced_tests/synthetic_hooks.rs"]
mod synthetic_hooks;
#[path = "graph_advanced_tests/workspace_setup.rs"]
mod workspace_setup;
