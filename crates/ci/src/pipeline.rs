//! Pipeline Utilities
//!
//! Common functions for filtering, expanding, and resolving pipeline tasks.
//! These utilities are shared across different CI emitters.

use crate::ir::Task;
use cuenv_core::ci::{MatrixTask, PipelineTask, TaskRef};
use cuenv_task_graph::compute_transitive_closure;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Filter IR tasks to only those needed by the pipeline.
///
/// This function:
/// 1. Expands task group references (prefix matches like `build.` → `build.linux`, `build.macos`)
/// 2. Resolves transitive dependencies
/// 3. Returns only the tasks that are needed
///
/// # Arguments
/// * `pipeline_tasks` - Task names/prefixes requested in the pipeline
/// * `ir_tasks` - All available IR tasks
///
/// # Returns
/// Filtered list of IR tasks that are needed for the pipeline
#[must_use]
pub fn filter_tasks(pipeline_tasks: &[String], ir_tasks: Vec<Task>) -> Vec<Task> {
    // Expand task group references: exact match or prefix expansion
    let expanded: HashSet<String> = pipeline_tasks
        .iter()
        .flat_map(|task_name| {
            let exact_match = ir_tasks.iter().any(|t| t.id == *task_name);
            if exact_match {
                vec![task_name.clone()]
            } else {
                // Expand as prefix (task group)
                let prefix = format!("{task_name}.");
                let matches: Vec<_> = ir_tasks
                    .iter()
                    .filter(|t| t.id.starts_with(&prefix))
                    .map(|t| t.id.clone())
                    .collect();
                if matches.is_empty() {
                    vec![task_name.clone()] // Keep original for dependency resolution
                } else {
                    matches
                }
            }
        })
        .collect();

    // Resolve transitive dependencies using centralized graph algorithm
    let deps: HashMap<&str, Vec<String>> = ir_tasks
        .iter()
        .map(|t| (t.id.as_str(), t.depends_on.clone()))
        .collect();

    let needed = compute_transitive_closure(expanded.iter().map(String::as_str), |name| {
        deps.get(name).map(|v| v.as_slice())
    });

    ir_tasks
        .into_iter()
        .filter(|t| needed.contains(t.id.as_str()))
        .collect()
}

/// Expand pipeline tasks that reference task groups into individual tasks.
///
/// If a task name doesn't match any IR task exactly, it's treated as a prefix
/// and all IR tasks starting with that prefix are included.
///
/// For matrix tasks, entry-point tasks (those with no internal dependencies)
/// inherit the matrix configuration, while dependent tasks become simple tasks.
///
/// # Arguments
/// * `pipeline_tasks` - Pipeline task definitions (Simple or Matrix)
/// * `ir_tasks` - All available IR tasks
/// * `explicit_task_names` - Task names explicitly listed in the pipeline (not expanded)
///
/// # Returns
/// Expanded list of pipeline tasks with task groups replaced by individual tasks
#[must_use]
pub fn expand_task_groups(
    pipeline_tasks: &[PipelineTask],
    ir_tasks: &[Task],
    explicit_task_names: &HashSet<String>,
) -> Vec<PipelineTask> {
    pipeline_tasks
        .iter()
        .flat_map(|pipeline_task| {
            let task_name = pipeline_task.task_name();

            // Check if this task exists in IR directly
            if ir_tasks.iter().any(|t| t.id == task_name) {
                return vec![pipeline_task.clone()];
            }

            // Not an exact match - expand as task group
            let prefix = format!("{task_name}.");
            let sub_tasks: Vec<_> = ir_tasks
                .iter()
                .filter(|t| t.id.starts_with(&prefix))
                .filter(|t| !explicit_task_names.contains(&t.id))
                .collect();

            if sub_tasks.is_empty() {
                return vec![pipeline_task.clone()];
            }

            // Entry-point tasks: those with no dependencies on other tasks in the same group
            let group_task_ids: HashSet<&str> = sub_tasks.iter().map(|t| t.id.as_str()).collect();

            sub_tasks
                .into_iter()
                .map(|ir_task| {
                    let has_internal_deps = ir_task
                        .depends_on
                        .iter()
                        .any(|dep| group_task_ids.contains(dep.as_str()));

                    match pipeline_task {
                        PipelineTask::Simple(_) => {
                            PipelineTask::Simple(TaskRef::from_name(&ir_task.id))
                        }
                        PipelineTask::Matrix(matrix_task) => {
                            if has_internal_deps {
                                PipelineTask::Simple(TaskRef::from_name(&ir_task.id))
                            } else {
                                // Empty matrix signals artifact aggregation mode
                                PipelineTask::Matrix(MatrixTask {
                                    task: TaskRef::from_name(&ir_task.id),
                                    artifacts: matrix_task.artifacts.clone(),
                                    params: matrix_task.params.clone(),
                                    matrix: BTreeMap::new(),
                                })
                            }
                        }
                    }
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::CachePolicy;
    use std::collections::BTreeMap;

    fn make_task(id: &str, depends_on: &[&str]) -> Task {
        Task {
            id: id.to_string(),
            runtime: None,
            command: vec!["echo".to_string()],
            shell: false,
            env: BTreeMap::new(),
            secrets: BTreeMap::new(),
            resources: None,
            concurrency_group: None,
            inputs: vec![],
            outputs: vec![],
            depends_on: depends_on.iter().map(|s| (*s).to_string()).collect(),
            cache_policy: CachePolicy::Normal,
            deployment: false,
            manual_approval: false,
            matrix: None,
            artifact_downloads: vec![],
            params: BTreeMap::new(),
            phase: None,
            label: None,
            priority: None,
            contributor: None,
            condition: None,
            provider_hints: None,
        }
    }

    #[test]
    fn test_transitive_closure_via_filter_tasks() {
        // Test transitive dependency resolution through filter_tasks
        // c -> b -> a (c depends on b, b depends on a)
        let tasks = vec![
            make_task("a", &[]),
            make_task("b", &["a"]),
            make_task("c", &["b"]),
        ];

        // Requesting just "c" should pull in b and a transitively
        let result = filter_tasks(&["c".to_string()], tasks);

        let ids: HashSet<_> = result.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains("a"));
        assert!(ids.contains("b"));
        assert!(ids.contains("c"));
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn test_filter_tasks_exact_match() {
        let tasks = vec![
            make_task("build", &[]),
            make_task("test", &["build"]),
            make_task("deploy", &["test"]),
        ];

        let result = filter_tasks(&["test".to_string()], tasks);

        // Should include test and its dependency (build)
        let ids: Vec<_> = result.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"build"));
        assert!(ids.contains(&"test"));
        assert!(!ids.contains(&"deploy"));
    }

    #[test]
    fn test_filter_tasks_prefix_expansion() {
        let tasks = vec![
            make_task("build.linux", &[]),
            make_task("build.macos", &[]),
            make_task("test", &["build.linux", "build.macos"]),
        ];

        // "build" prefix should expand to build.linux and build.macos
        let result = filter_tasks(&["build".to_string()], tasks);

        let ids: Vec<_> = result.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"build.linux"));
        assert!(ids.contains(&"build.macos"));
        assert!(!ids.contains(&"test"));
    }

    #[test]
    fn test_expand_task_groups_simple() {
        let ir_tasks = vec![make_task("build.linux", &[]), make_task("build.macos", &[])];
        let pipeline_tasks = vec![PipelineTask::Simple(TaskRef::from_name("build"))];
        let explicit: HashSet<String> = HashSet::new();

        let result = expand_task_groups(&pipeline_tasks, &ir_tasks, &explicit);

        assert_eq!(result.len(), 2);
        let names: Vec<_> = result.iter().map(|t| t.task_name()).collect();
        assert!(names.contains(&"build.linux"));
        assert!(names.contains(&"build.macos"));
    }

    #[test]
    fn test_expand_task_groups_matrix_inheritance() {
        let ir_tasks = vec![
            make_task("build.linux", &[]),
            make_task("build.macos", &["build.linux"]), // has internal dep
        ];
        let pipeline_tasks = vec![PipelineTask::Matrix(MatrixTask {
            task: TaskRef::from_name("build"),
            artifacts: None,
            params: None,
            matrix: [("os".to_string(), vec!["linux".to_string()])]
                .into_iter()
                .collect(),
        })];
        let explicit: HashSet<String> = HashSet::new();

        let result = expand_task_groups(&pipeline_tasks, &ir_tasks, &explicit);

        assert_eq!(result.len(), 2);

        // build.linux should be Matrix (no internal deps - entry point)
        // build.macos should be Simple (has internal dep)
        for task in &result {
            match task {
                PipelineTask::Matrix(m) if m.task.task_name() == "build.linux" => {
                    // Entry point gets empty matrix (artifact aggregation mode)
                    assert!(m.matrix.is_empty());
                }
                PipelineTask::Simple(task_ref) if task_ref.task_name() == "build.macos" => {
                    // Has internal dep, becomes Simple
                }
                _ => panic!("Unexpected task configuration: {:?}", task),
            }
        }
    }
}
