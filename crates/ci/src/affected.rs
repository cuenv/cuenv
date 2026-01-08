use cuenv_core::manifest::Project;
use cuenv_core::tasks::TaskIndex;
use cuenv_core::{AffectedBy, matches_pattern};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn compute_affected_tasks(
    changed_files: &[PathBuf],
    pipeline_tasks: &[String],
    project_root: &Path,
    config: &Project,
    all_projects: &HashMap<String, (PathBuf, Project)>,
) -> Vec<String> {
    let mut affected = HashSet::new();
    let mut visited_external_cache: HashMap<String, bool> = HashMap::new();

    // 1. Identify directly affected tasks (file changes in this project)
    for task_name in pipeline_tasks {
        if is_task_directly_affected(task_name, config, changed_files, project_root) {
            affected.insert(task_name.clone());
        }
    }

    // 2. Transitive dependencies
    // We need to check dependencies recursively including cross-project ones
    // Build task index once for resolving nested task names
    let Ok(index) = TaskIndex::build(&config.tasks) else {
        return pipeline_tasks
            .iter()
            .filter(|t| affected.contains(*t))
            .cloned()
            .collect();
    };

    let mut changed = true;
    while changed {
        changed = false;
        for task_name in pipeline_tasks {
            if affected.contains(task_name) {
                continue;
            }

            if let Ok(entry) = index.resolve(task_name)
                && let Some(task) = entry.node.as_task()
                && !task.depends_on.is_empty()
            {
                for dep in &task.depends_on {
                    let dep_name = dep.task_name();
                    // Internal dependency
                    if !dep_name.starts_with('#') {
                        if affected.contains(dep_name) {
                            affected.insert(task_name.clone());
                            changed = true;
                            break;
                        }
                        continue;
                    }

                    // External dependency (#project:task) - no longer supported
                    // Keeping check for safety but this path shouldn't be hit
                    if check_external_dependency(
                        dep_name,
                        all_projects,
                        changed_files,
                        &mut visited_external_cache,
                    ) {
                        affected.insert(task_name.clone());
                        changed = true;
                        break;
                    }
                }
            }
        }
    }

    // Return in pipeline order
    pipeline_tasks
        .iter()
        .filter(|t| affected.contains(*t))
        .cloned()
        .collect()
}

#[must_use]
pub fn matched_inputs_for_task(
    task_name: &str,
    config: &Project,
    changed_files: &[PathBuf],
    project_root: &Path,
) -> Vec<String> {
    // Build task index to resolve nested names like "deploy.preview"
    let Ok(index) = TaskIndex::build(&config.tasks) else {
        return Vec::new();
    };

    let Ok(entry) = index.resolve(task_name) else {
        return Vec::new();
    };

    let Some(task) = entry.node.as_task() else {
        return Vec::new();
    };

    task.iter_path_inputs()
        .filter(|input_glob| matches_pattern(changed_files, project_root, input_glob))
        .cloned()
        .collect()
}

/// Check if a task is directly affected by file changes.
///
/// Uses the [`AffectedBy`] trait implementation from cuenv-core, which handles:
/// - Single tasks: affected if any input pattern matches changed files
/// - Task groups: affected if ANY subtask is affected
/// - Tasks with no inputs: always considered affected (safe default)
///
/// This function uses `TaskIndex` to resolve nested task names like "deploy.preview"
/// which are stored in CUE as hierarchical structures (e.g., `deploy: preview: {...}`).
fn is_task_directly_affected(
    task_name: &str,
    config: &Project,
    changed_files: &[PathBuf],
    project_root: &Path,
) -> bool {
    // Build task index to resolve nested names like "deploy.preview"
    let Ok(index) = TaskIndex::build(&config.tasks) else {
        return false;
    };

    index
        .resolve(task_name)
        .ok()
        .is_some_and(|entry| entry.node.is_affected_by(changed_files, project_root))
}

/// Check if an external dependency (cross-project task) is affected by file changes.
///
/// External dependencies are specified in the format `#project:task`. This function
/// recursively checks if the referenced task or any of its dependencies are affected.
///
/// # Recursion Prevention
///
/// To prevent infinite loops with circular dependencies, we insert a `false` sentinel
/// value into the cache before checking. If we encounter this dependency again during
/// recursion, we return false (not affected). Once the check completes, the cache is
/// updated with the actual result.
#[allow(clippy::implicit_hasher)]
fn check_external_dependency(
    dep: &str,
    all_projects: &HashMap<String, (PathBuf, Project)>,
    changed_files: &[PathBuf],
    cache: &mut HashMap<String, bool>,
) -> bool {
    // dep format: "#project:task"
    if let Some(result) = cache.get(dep) {
        return *result;
    }

    // Insert false as a sentinel to prevent infinite recursion on circular dependencies.
    // This will be updated with the actual result once the check completes.
    cache.insert(dep.to_string(), false);

    let parts: Vec<&str> = dep[1..].split(':').collect();
    if parts.len() < 2 {
        return false;
    }
    let project_name = parts[0];
    let task_name = parts[1];

    let Some((project_path, project_config)) = all_projects.get(project_name) else {
        return false;
    };

    // Check if directly affected
    if is_task_directly_affected(task_name, project_config, changed_files, project_path) {
        cache.insert(dep.to_string(), true);
        return true;
    }

    // Check transitive dependencies of the external task
    // Use TaskIndex to resolve nested task names
    let Ok(index) = TaskIndex::build(&project_config.tasks) else {
        return false;
    };
    if let Ok(entry) = index.resolve(task_name)
        && let Some(task) = entry.node.as_task()
    {
        for sub_dep in &task.depends_on {
            let sub_dep_name = sub_dep.task_name();
            if sub_dep_name.starts_with('#') {
                // External ref - no longer supported but keeping check for safety
                if check_external_dependency(sub_dep_name, all_projects, changed_files, cache) {
                    cache.insert(dep.to_string(), true);
                    return true;
                }
            } else {
                // Internal ref within that project
                // We need to resolve internal deps of the external project recursively.
                // Construct implicit external ref: #project:sub_dep
                let implicit_ref = format!("#{project_name}:{sub_dep_name}");
                if check_external_dependency(&implicit_ref, all_projects, changed_files, cache) {
                    cache.insert(dep.to_string(), true);
                    return true;
                }
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::manifest::Project;
    use cuenv_core::tasks::{Input, Task, TaskDependency, TaskGroup, TaskNode};

    /// Helper to create a minimal Project with tasks
    fn make_project(tasks: Vec<(&str, Task)>) -> Project {
        let mut project = Project::default();
        for (name, task) in tasks {
            project
                .tasks
                .insert(name.to_string(), TaskNode::Task(Box::new(task)));
        }
        project
    }

    /// Helper to create a minimal Task with inputs and depends_on
    fn make_task(inputs: Vec<&str>, depends_on: Vec<&str>) -> Task {
        Task {
            inputs: inputs
                .into_iter()
                .map(|s| Input::Path(s.to_string()))
                .collect(),
            depends_on: depends_on
                .into_iter()
                .map(TaskDependency::from_name)
                .collect(),
            command: "echo test".to_string(),
            ..Default::default()
        }
    }

    // NOTE: Pattern matching tests are now in cuenv-core/src/affected.rs
    // Tests below focus on CI-specific logic.

    // ==========================================================================
    // matched_inputs_for_task tests
    // ==========================================================================

    #[test]
    fn test_matched_inputs_for_task_returns_matching_patterns() {
        let task = make_task(vec!["src/**", "Cargo.toml"], vec![]);
        let project = make_project(vec![("build", task)]);
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        let matched = matched_inputs_for_task("build", &project, &changed_files, root);

        assert_eq!(matched, vec!["src/**".to_string()]);
    }

    #[test]
    fn test_matched_inputs_for_task_no_match() {
        let task = make_task(vec!["src/**"], vec![]);
        let project = make_project(vec![("build", task)]);
        let changed_files = vec![PathBuf::from("tests/test.rs")];
        let root = Path::new(".");

        let matched = matched_inputs_for_task("build", &project, &changed_files, root);

        assert!(matched.is_empty());
    }

    #[test]
    fn test_matched_inputs_for_task_nonexistent_task() {
        let project = Project::default();
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        let matched = matched_inputs_for_task("nonexistent", &project, &changed_files, root);

        assert!(matched.is_empty());
    }

    #[test]
    fn test_matched_inputs_for_task_multiple_matches() {
        let task = make_task(vec!["src/**", "lib/**", "Cargo.toml"], vec![]);
        let project = make_project(vec![("build", task)]);
        let changed_files = vec![PathBuf::from("src/lib.rs"), PathBuf::from("lib/util.rs")];
        let root = Path::new(".");

        let matched = matched_inputs_for_task("build", &project, &changed_files, root);

        assert!(matched.contains(&"src/**".to_string()));
        assert!(matched.contains(&"lib/**".to_string()));
        assert!(!matched.contains(&"Cargo.toml".to_string()));
    }

    // ==========================================================================
    // compute_affected_tasks tests
    // ==========================================================================

    #[test]
    fn test_compute_affected_tasks_direct_match() {
        let task = make_task(vec!["src/**"], vec![]);
        let project = make_project(vec![("build", task)]);
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");
        let pipeline_tasks = vec!["build".to_string()];
        let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            root,
            &project,
            &all_projects,
        );

        assert_eq!(affected, vec!["build".to_string()]);
    }

    #[test]
    fn test_compute_affected_tasks_no_match() {
        let task = make_task(vec!["src/**"], vec![]);
        let project = make_project(vec![("build", task)]);
        let changed_files = vec![PathBuf::from("docs/readme.md")];
        let root = Path::new(".");
        let pipeline_tasks = vec!["build".to_string()];
        let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            root,
            &project,
            &all_projects,
        );

        assert!(affected.is_empty());
    }

    #[test]
    fn test_compute_affected_tasks_transitive_internal_deps() {
        // test depends on build, build is affected -> test should also be affected
        let build_task = make_task(vec!["src/**"], vec![]);
        let test_task = make_task(vec![], vec!["build"]);
        let project = make_project(vec![("build", build_task), ("test", test_task)]);
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");
        let pipeline_tasks = vec!["build".to_string(), "test".to_string()];
        let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            root,
            &project,
            &all_projects,
        );

        assert!(affected.contains(&"build".to_string()));
        assert!(affected.contains(&"test".to_string()));
    }

    #[test]
    fn test_compute_affected_tasks_preserves_pipeline_order() {
        let build_task = make_task(vec!["src/**"], vec![]);
        let test_task = make_task(vec![], vec!["build"]);
        let deploy_task = make_task(vec![], vec!["test"]);
        let project = make_project(vec![
            ("build", build_task),
            ("test", test_task),
            ("deploy", deploy_task),
        ]);
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");
        // Pipeline order: build, test, deploy
        let pipeline_tasks = vec![
            "build".to_string(),
            "test".to_string(),
            "deploy".to_string(),
        ];
        let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            root,
            &project,
            &all_projects,
        );

        // Should be in pipeline order
        assert_eq!(affected, vec!["build", "test", "deploy"]);
    }

    #[test]
    fn test_compute_affected_tasks_only_affected_in_pipeline() {
        // If a task is not in pipeline_tasks, it shouldn't be in the result
        let build_task = make_task(vec!["src/**"], vec![]);
        let test_task = make_task(vec![], vec!["build"]);
        let project = make_project(vec![("build", build_task), ("test", test_task)]);
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");
        // Only build in the pipeline, not test
        let pipeline_tasks = vec!["build".to_string()];
        let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            root,
            &project,
            &all_projects,
        );

        // Only build should be returned (test not in pipeline)
        assert_eq!(affected, vec!["build"]);
    }

    #[test]
    fn test_compute_affected_tasks_empty_pipeline() {
        let task = make_task(vec!["src/**"], vec![]);
        let project = make_project(vec![("build", task)]);
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");
        let pipeline_tasks: Vec<String> = vec![];
        let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            root,
            &project,
            &all_projects,
        );

        assert!(affected.is_empty());
    }

    #[test]
    fn test_compute_affected_tasks_external_dep_not_found() {
        // External dependency to non-existent project should be skipped
        // Task has inputs so it won't be auto-affected
        let task = make_task(vec!["deploy/**"], vec!["#nonexistent:build"]);
        let project = make_project(vec![("deploy", task)]);
        let changed_files = vec![PathBuf::from("src/lib.rs")]; // Doesn't match deploy/**
        let root = Path::new(".");
        let pipeline_tasks = vec!["deploy".to_string()];
        let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            root,
            &project,
            &all_projects,
        );

        // deploy shouldn't be affected because:
        // 1. Its inputs don't match the changed files
        // 2. Its external dep project doesn't exist
        assert!(affected.is_empty());
    }

    #[test]
    fn test_compute_affected_tasks_external_dep_affected() {
        // External dependency is affected -> task should be affected
        let external_build = make_task(vec!["src/**"], vec![]);
        let mut external_project = Project::default();
        external_project.tasks.insert(
            "build".to_string(),
            TaskNode::Task(Box::new(external_build)),
        );

        let deploy_task = make_task(vec![], vec!["#external:build"]);
        let project = make_project(vec![("deploy", deploy_task)]);

        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");
        let pipeline_tasks = vec!["deploy".to_string()];

        let mut all_projects = HashMap::new();
        all_projects.insert(
            "external".to_string(),
            (PathBuf::from("/repo/external"), external_project),
        );

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            root,
            &project,
            &all_projects,
        );

        assert!(affected.contains(&"deploy".to_string()));
    }

    #[test]
    fn test_compute_affected_tasks_malformed_external_dep() {
        // Malformed external dependency (missing colon) should be skipped
        // Task has inputs so it won't be auto-affected
        let task = make_task(vec!["deploy/**"], vec!["#badformat"]);
        let project = make_project(vec![("deploy", task)]);
        let changed_files = vec![PathBuf::from("src/lib.rs")]; // Doesn't match deploy/**
        let root = Path::new(".");
        let pipeline_tasks = vec!["deploy".to_string()];
        let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            root,
            &project,
            &all_projects,
        );

        // deploy shouldn't be affected because:
        // 1. Its inputs don't match the changed files
        // 2. Its malformed external dep is skipped
        assert!(affected.is_empty());
    }

    // ==========================================================================
    // is_task_directly_affected tests
    // ==========================================================================

    #[test]
    fn test_is_task_directly_affected_match() {
        let task = make_task(vec!["src/**"], vec![]);
        let project = make_project(vec![("build", task)]);
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        assert!(is_task_directly_affected(
            "build",
            &project,
            &changed_files,
            root
        ));
    }

    #[test]
    fn test_is_task_directly_affected_no_match() {
        let task = make_task(vec!["src/**"], vec![]);
        let project = make_project(vec![("build", task)]);
        let changed_files = vec![PathBuf::from("docs/readme.md")];
        let root = Path::new(".");

        assert!(!is_task_directly_affected(
            "build",
            &project,
            &changed_files,
            root
        ));
    }

    #[test]
    fn test_is_task_directly_affected_nonexistent_task() {
        let project = Project::default();
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        assert!(!is_task_directly_affected(
            "nonexistent",
            &project,
            &changed_files,
            root
        ));
    }

    #[test]
    fn test_is_task_directly_affected_task_no_inputs_always_affected() {
        // Tasks with no inputs should always be considered affected
        // because we can't determine what affects them
        let task = make_task(vec![], vec![]);
        let project = make_project(vec![("build", task)]);
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        assert!(is_task_directly_affected(
            "build",
            &project,
            &changed_files,
            root
        ));
    }

    #[test]
    fn test_task_with_no_inputs_always_affected_even_with_no_changes() {
        // Even when no files changed, a task with no inputs should be affected
        let task = make_task(vec![], vec![]);
        let project = make_project(vec![("deploy", task)]);
        let changed_files: Vec<PathBuf> = vec![]; // No changes
        let root = Path::new(".");
        let pipeline_tasks = vec!["deploy".to_string()];
        let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            root,
            &project,
            &all_projects,
        );

        assert_eq!(
            affected,
            vec!["deploy"],
            "Task with no inputs should always be affected"
        );
    }

    // ==========================================================================
    // Task group affected detection tests
    // ==========================================================================

    #[test]
    fn test_parallel_group_one_subtask_affected() {
        // check group: { lint: inputs: ["src/**"], test: inputs: ["tests/**"] }
        // Changed file: src/lib.rs -> lint is affected -> group is affected
        let lint_task = make_task(vec!["src/**"], vec![]);
        let test_task = make_task(vec!["tests/**"], vec![]);

        let mut parallel_tasks = std::collections::HashMap::new();
        parallel_tasks.insert("lint".to_string(), TaskNode::Task(Box::new(lint_task)));
        parallel_tasks.insert("test".to_string(), TaskNode::Task(Box::new(test_task)));

        let group = TaskGroup {
            type_: "group".to_string(),
            children: parallel_tasks,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        };

        let mut project = Project::default();
        project
            .tasks
            .insert("check".to_string(), TaskNode::Group(group));

        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        assert!(is_task_directly_affected(
            "check",
            &project,
            &changed_files,
            root
        ));
    }

    #[test]
    fn test_parallel_group_no_subtask_affected() {
        // check group: { lint: inputs: ["src/**"], test: inputs: ["tests/**"] }
        // Changed file: docs/readme.md -> no subtask affected -> group not affected
        let lint_task = make_task(vec!["src/**"], vec![]);
        let test_task = make_task(vec!["tests/**"], vec![]);

        let mut parallel_tasks = std::collections::HashMap::new();
        parallel_tasks.insert("lint".to_string(), TaskNode::Task(Box::new(lint_task)));
        parallel_tasks.insert("test".to_string(), TaskNode::Task(Box::new(test_task)));

        let group = TaskGroup {
            type_: "group".to_string(),
            children: parallel_tasks,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        };

        let mut project = Project::default();
        project
            .tasks
            .insert("check".to_string(), TaskNode::Group(group));

        let changed_files = vec![PathBuf::from("docs/readme.md")];
        let root = Path::new(".");

        assert!(!is_task_directly_affected(
            "check",
            &project,
            &changed_files,
            root
        ));
    }

    #[test]
    fn test_sequential_group_affected() {
        // Sequential group: [lint, test] where lint is affected
        let lint_task = make_task(vec!["src/**"], vec![]);
        let test_task = make_task(vec!["tests/**"], vec![]);

        let seq = TaskNode::Sequence(vec![
            TaskNode::Task(Box::new(lint_task)),
            TaskNode::Task(Box::new(test_task)),
        ]);

        let mut project = Project::default();
        project.tasks.insert("check".to_string(), seq);

        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        assert!(is_task_directly_affected(
            "check",
            &project,
            &changed_files,
            root
        ));
    }

    #[test]
    fn test_compute_affected_tasks_with_group() {
        // Pipeline has ["check"] where check is a group containing lint (affected)
        let lint_task = make_task(vec!["src/**"], vec![]);
        let test_task = make_task(vec!["tests/**"], vec![]);

        let mut parallel_tasks = std::collections::HashMap::new();
        parallel_tasks.insert("lint".to_string(), TaskNode::Task(Box::new(lint_task)));
        parallel_tasks.insert("test".to_string(), TaskNode::Task(Box::new(test_task)));

        let group = TaskGroup {
            type_: "group".to_string(),
            children: parallel_tasks,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        };

        let mut project = Project::default();
        project
            .tasks
            .insert("check".to_string(), TaskNode::Group(group));

        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");
        let pipeline_tasks = vec!["check".to_string()];
        let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            root,
            &project,
            &all_projects,
        );

        // "check" should be in affected list because its "lint" subtask is affected
        assert_eq!(affected, vec!["check".to_string()]);
    }

    // ==========================================================================
    // check_external_dependency tests
    // ==========================================================================

    #[test]
    fn test_check_external_dependency_cache_hit() {
        let mut cache = HashMap::new();
        cache.insert("#project:task".to_string(), true);
        let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();
        let changed_files: Vec<PathBuf> = vec![];

        let result =
            check_external_dependency("#project:task", &all_projects, &changed_files, &mut cache);

        assert!(result);
    }

    #[test]
    fn test_check_external_dependency_cache_miss_false() {
        let mut cache = HashMap::new();
        cache.insert("#project:task".to_string(), false);
        let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();
        let changed_files: Vec<PathBuf> = vec![];

        let result =
            check_external_dependency("#project:task", &all_projects, &changed_files, &mut cache);

        assert!(!result);
    }

    #[test]
    fn test_check_external_dependency_project_not_found() {
        let mut cache = HashMap::new();
        let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();
        let changed_files = vec![PathBuf::from("src/lib.rs")];

        let result =
            check_external_dependency("#missing:task", &all_projects, &changed_files, &mut cache);

        assert!(!result);
    }

    #[test]
    fn test_check_external_dependency_directly_affected() {
        let external_build = make_task(vec!["src/**"], vec![]);
        let mut external_project = Project::default();
        external_project.tasks.insert(
            "build".to_string(),
            TaskNode::Task(Box::new(external_build)),
        );

        let mut all_projects = HashMap::new();
        all_projects.insert(
            "external".to_string(),
            (PathBuf::from("/repo/external"), external_project),
        );

        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let mut cache = HashMap::new();

        let result =
            check_external_dependency("#external:build", &all_projects, &changed_files, &mut cache);

        assert!(result);
        assert_eq!(cache.get("#external:build"), Some(&true));
    }

    #[test]
    fn test_check_external_dependency_transitive_internal() {
        // External project has: test depends on build, build is affected
        // -> #external:test should be affected
        let external_build = make_task(vec!["src/**"], vec![]);
        let external_test = make_task(vec![], vec!["build"]);
        let mut external_project = Project::default();
        external_project.tasks.insert(
            "build".to_string(),
            TaskNode::Task(Box::new(external_build)),
        );
        external_project
            .tasks
            .insert("test".to_string(), TaskNode::Task(Box::new(external_test)));

        let mut all_projects = HashMap::new();
        all_projects.insert(
            "external".to_string(),
            (PathBuf::from("/repo/external"), external_project),
        );

        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let mut cache = HashMap::new();

        let result =
            check_external_dependency("#external:test", &all_projects, &changed_files, &mut cache);

        assert!(result);
    }

    #[test]
    fn test_check_external_dependency_circular_prevention() {
        // Task A depends on itself (circular) - should not infinite loop
        // Task has inputs so it won't be auto-affected
        let circular_task = make_task(vec!["taskA/**"], vec!["#proj:taskA"]);
        let mut project = Project::default();
        project
            .tasks
            .insert("taskA".to_string(), TaskNode::Task(Box::new(circular_task)));

        let mut all_projects = HashMap::new();
        all_projects.insert("proj".to_string(), (PathBuf::from("/repo/proj"), project));

        let changed_files: Vec<PathBuf> = vec![]; // No changes matching taskA/**
        let mut cache = HashMap::new();

        // Should return false without infinite loop
        // (inputs don't match and circular dep doesn't cause issues)
        let result =
            check_external_dependency("#proj:taskA", &all_projects, &changed_files, &mut cache);

        assert!(!result);
    }
}
