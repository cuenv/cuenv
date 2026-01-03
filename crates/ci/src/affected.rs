use cuenv_core::manifest::Project;
use cuenv_core::tasks::{TaskDefinition, TaskGroup};
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
    let mut changed = true;
    while changed {
        changed = false;
        for task_name in pipeline_tasks {
            if affected.contains(task_name) {
                continue;
            }

            if let Some(task_def) = config.tasks.get(task_name)
                && let Some(task) = task_def.as_single()
                && !task.depends_on.is_empty()
            {
                for dep in &task.depends_on {
                    // Internal dependency
                    if !dep.starts_with('#') {
                        if affected.contains(dep) {
                            affected.insert(task_name.clone());
                            changed = true;
                            break;
                        }
                        continue;
                    }

                    // External dependency (#project:task)
                    if check_external_dependency(
                        dep,
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
    let Some(task_def) = config.tasks.get(task_name) else {
        return Vec::new();
    };
    let Some(task) = task_def.as_single() else {
        return Vec::new();
    };
    task.iter_path_inputs()
        .filter(|input_glob| matches_any(changed_files, project_root, input_glob))
        .cloned()
        .collect()
}

/// Check if a task definition is directly affected by file changes.
///
/// Handles both single tasks and task groups (parallel/sequential).
/// For groups, returns true if ANY subtask is affected.
fn is_definition_affected(
    def: &TaskDefinition,
    changed_files: &[PathBuf],
    project_root: &Path,
) -> bool {
    match def {
        TaskDefinition::Single(task) => task
            .iter_path_inputs()
            .any(|input_glob| matches_any(changed_files, project_root, input_glob)),
        TaskDefinition::Group(group) => is_group_affected(group, changed_files, project_root),
    }
}

/// Check if a task group is directly affected by file changes.
///
/// A group is affected if ANY of its subtasks are affected.
fn is_group_affected(group: &TaskGroup, changed_files: &[PathBuf], project_root: &Path) -> bool {
    match group {
        TaskGroup::Parallel(parallel) => parallel
            .tasks
            .values()
            .any(|def| is_definition_affected(def, changed_files, project_root)),
        TaskGroup::Sequential(tasks) => tasks
            .iter()
            .any(|def| is_definition_affected(def, changed_files, project_root)),
    }
}

/// Check if a task is directly affected by file changes.
///
/// A task is directly affected if any of its input patterns match any of the changed files.
/// For task groups, returns true if any subtask in the group is affected.
fn is_task_directly_affected(
    task_name: &str,
    config: &Project,
    changed_files: &[PathBuf],
    project_root: &Path,
) -> bool {
    let Some(task_def) = config.tasks.get(task_name) else {
        return false;
    };

    is_definition_affected(task_def, changed_files, project_root)
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
    if let Some(task_def) = project_config.tasks.get(task_name)
        && let Some(task) = task_def.as_single()
    {
        for sub_dep in &task.depends_on {
            if sub_dep.starts_with('#') {
                // External ref
                if check_external_dependency(sub_dep, all_projects, changed_files, cache) {
                    cache.insert(dep.to_string(), true);
                    return true;
                }
            } else {
                // Internal ref within that project
                // We need to resolve internal deps of the external project recursively.
                // Construct implicit external ref: #project:sub_dep
                let implicit_ref = format!("#{project_name}:{sub_dep}");
                if check_external_dependency(&implicit_ref, all_projects, changed_files, cache) {
                    cache.insert(dep.to_string(), true);
                    return true;
                }
            }
        }
    }

    false
}

/// Check if any of the given files match a pattern.
///
/// Supports two matching modes:
/// - **Simple paths**: Patterns without wildcards (`*`, `?`, `[`) are treated as path prefixes.
///   For example, `"crates"` matches `"crates/foo/bar.rs"`.
/// - **Glob patterns**: Patterns with wildcards use glob matching.
///
/// File paths are normalized relative to the project root before matching.
fn matches_any(files: &[PathBuf], root: &Path, pattern: &str) -> bool {
    // If pattern doesn't contain glob characters, treat it as a path prefix
    // e.g., "crates" should match "crates/foo/bar.rs"
    let is_simple_path = !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('[');

    for file in files {
        // Get relative path for matching:
        // - If root is "." or empty, use file as-is
        // - If file is already relative (doesn't start with root), use it as-is
        //   (git returns relative paths, project_root may be absolute)
        // - Otherwise strip the root prefix
        let relative_path = if root == Path::new(".") || root.as_os_str().is_empty() {
            file.as_path()
        } else if file.is_relative() {
            // File is already relative (e.g., from git diff), use as-is
            file.as_path()
        } else {
            match file.strip_prefix(root) {
                Ok(p) => p,
                Err(_) => continue,
            }
        };

        if is_simple_path {
            // Check if the pattern is a prefix of the file path.
            // Note: starts_with includes exact matches, so no separate equality check needed.
            let pattern_path = Path::new(pattern);
            if relative_path.starts_with(pattern_path) {
                return true;
            }
        } else {
            // Use glob matching for patterns with wildcards
            let Ok(glob) = glob::Pattern::new(pattern) else {
                tracing::trace!(pattern, "Skipping invalid glob pattern");
                continue;
            };
            if glob.matches_path(relative_path) {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::manifest::Project;
    use cuenv_core::tasks::{Input, ParallelGroup, Task, TaskDefinition, TaskGroup};

    /// Helper to create a minimal Project with tasks
    fn make_project(tasks: Vec<(&str, Task)>) -> Project {
        let mut project = Project::default();
        for (name, task) in tasks {
            project
                .tasks
                .insert(name.to_string(), TaskDefinition::Single(Box::new(task)));
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
            depends_on: depends_on.into_iter().map(String::from).collect(),
            command: "echo test".to_string(),
            ..Default::default()
        }
    }

    // ==========================================================================
    // matches_any tests
    // ==========================================================================

    #[test]
    fn test_matches_any_simple_prefix_match() {
        let files = vec![PathBuf::from("crates/foo/bar.rs")];
        let root = Path::new(".");

        assert!(matches_any(&files, root, "crates"));
        assert!(matches_any(&files, root, "crates/foo"));
        assert!(matches_any(&files, root, "crates/foo/bar.rs"));
    }

    #[test]
    fn test_matches_any_no_match() {
        let files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        assert!(!matches_any(&files, root, "crates"));
        assert!(!matches_any(&files, root, "tests"));
    }

    #[test]
    fn test_matches_any_glob_pattern() {
        let files = vec![
            PathBuf::from("src/lib.rs"),
            PathBuf::from("src/main.rs"),
            PathBuf::from("tests/test.rs"),
        ];
        let root = Path::new(".");

        assert!(matches_any(&files, root, "*.rs"));
        assert!(matches_any(&files, root, "src/*.rs"));
        assert!(!matches_any(&files, root, "*.txt"));
    }

    #[test]
    fn test_matches_any_glob_with_question_mark() {
        let files = vec![PathBuf::from("src/a.rs"), PathBuf::from("src/ab.rs")];
        let root = Path::new(".");

        assert!(matches_any(&files, root, "src/?.rs"));
        assert!(!matches_any(&files, root, "src/???.rs"));
    }

    #[test]
    fn test_matches_any_glob_with_brackets() {
        let files = vec![PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")];
        let root = Path::new(".");

        assert!(matches_any(&files, root, "src/[ab].rs"));
        assert!(!matches_any(&files, root, "src/[cd].rs"));
    }

    #[test]
    fn test_matches_any_with_absolute_paths() {
        let files = vec![PathBuf::from("/project/src/lib.rs")];
        let root = Path::new("/project");

        assert!(matches_any(&files, root, "src"));
        assert!(matches_any(&files, root, "src/lib.rs"));
    }

    #[test]
    fn test_matches_any_relative_files_absolute_root() {
        // Files from git diff are relative, but project_root may be absolute
        let files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new("/some/absolute/path");

        // Should still match because file is relative
        assert!(matches_any(&files, root, "src"));
    }

    #[test]
    fn test_matches_any_empty_root() {
        let files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new("");

        assert!(matches_any(&files, root, "src"));
    }

    #[test]
    fn test_matches_any_empty_files() {
        let files: Vec<PathBuf> = vec![];
        let root = Path::new(".");

        assert!(!matches_any(&files, root, "src"));
    }

    #[test]
    fn test_matches_any_invalid_glob_pattern() {
        let files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        // Invalid glob pattern should be skipped (returns false)
        assert!(!matches_any(&files, root, "[invalid"));
    }

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
        let task = make_task(vec![], vec!["#nonexistent:build"]);
        let project = make_project(vec![("deploy", task)]);
        let changed_files = vec![PathBuf::from("src/lib.rs")];
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

        // deploy shouldn't be affected because its dep project doesn't exist
        assert!(affected.is_empty());
    }

    #[test]
    fn test_compute_affected_tasks_external_dep_affected() {
        // External dependency is affected -> task should be affected
        let external_build = make_task(vec!["src/**"], vec![]);
        let mut external_project = Project::default();
        external_project.tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(external_build)),
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
        let task = make_task(vec![], vec!["#badformat"]);
        let project = make_project(vec![("deploy", task)]);
        let changed_files = vec![PathBuf::from("src/lib.rs")];
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

        // Malformed external dep is skipped
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
    fn test_is_task_directly_affected_task_no_inputs() {
        let task = make_task(vec![], vec![]);
        let project = make_project(vec![("build", task)]);
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        assert!(!is_task_directly_affected(
            "build",
            &project,
            &changed_files,
            root
        ));
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
        parallel_tasks.insert(
            "lint".to_string(),
            TaskDefinition::Single(Box::new(lint_task)),
        );
        parallel_tasks.insert(
            "test".to_string(),
            TaskDefinition::Single(Box::new(test_task)),
        );

        let group = TaskGroup::Parallel(ParallelGroup {
            tasks: parallel_tasks,
            depends_on: vec![],
        });

        let mut project = Project::default();
        project
            .tasks
            .insert("check".to_string(), TaskDefinition::Group(group));

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
        parallel_tasks.insert(
            "lint".to_string(),
            TaskDefinition::Single(Box::new(lint_task)),
        );
        parallel_tasks.insert(
            "test".to_string(),
            TaskDefinition::Single(Box::new(test_task)),
        );

        let group = TaskGroup::Parallel(ParallelGroup {
            tasks: parallel_tasks,
            depends_on: vec![],
        });

        let mut project = Project::default();
        project
            .tasks
            .insert("check".to_string(), TaskDefinition::Group(group));

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

        let group = TaskGroup::Sequential(vec![
            TaskDefinition::Single(Box::new(lint_task)),
            TaskDefinition::Single(Box::new(test_task)),
        ]);

        let mut project = Project::default();
        project
            .tasks
            .insert("check".to_string(), TaskDefinition::Group(group));

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
        parallel_tasks.insert(
            "lint".to_string(),
            TaskDefinition::Single(Box::new(lint_task)),
        );
        parallel_tasks.insert(
            "test".to_string(),
            TaskDefinition::Single(Box::new(test_task)),
        );

        let group = TaskGroup::Parallel(ParallelGroup {
            tasks: parallel_tasks,
            depends_on: vec![],
        });

        let mut project = Project::default();
        project
            .tasks
            .insert("check".to_string(), TaskDefinition::Group(group));

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
            TaskDefinition::Single(Box::new(external_build)),
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
            TaskDefinition::Single(Box::new(external_build)),
        );
        external_project.tasks.insert(
            "test".to_string(),
            TaskDefinition::Single(Box::new(external_test)),
        );

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
        let circular_task = make_task(vec![], vec!["#proj:taskA"]);
        let mut project = Project::default();
        project.tasks.insert(
            "taskA".to_string(),
            TaskDefinition::Single(Box::new(circular_task)),
        );

        let mut all_projects = HashMap::new();
        all_projects.insert("proj".to_string(), (PathBuf::from("/repo/proj"), project));

        let changed_files: Vec<PathBuf> = vec![];
        let mut cache = HashMap::new();

        // Should return false without infinite loop
        let result =
            check_external_dependency("#proj:taskA", &all_projects, &changed_files, &mut cache);

        assert!(!result);
    }
}
