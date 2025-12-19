use crate::discovery::DiscoveredCIProject;
use cuenv_core::manifest::Project;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[must_use]
#[allow(clippy::implicit_hasher)]
pub fn compute_affected_tasks(
    changed_files: &[PathBuf],
    pipeline_tasks: &[String],
    project_root: &Path,
    config: &Project,
    all_projects: &HashMap<String, DiscoveredCIProject>,
) -> Vec<String> {
    let mut affected = HashSet::new();
    let mut directly_affected = HashSet::new();
    let mut visited_external_cache: HashMap<String, bool> = HashMap::new();

    // 1. Identify directly affected tasks (file changes in this project)
    for task_name in pipeline_tasks {
        if is_task_directly_affected(task_name, config, changed_files, project_root) {
            directly_affected.insert(task_name.clone());
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

fn is_task_directly_affected(
    task_name: &str,
    config: &Project,
    changed_files: &[PathBuf],
    project_root: &Path,
) -> bool {
    if let Some(task_def) = config.tasks.get(task_name)
        && let Some(task) = task_def.as_single()
    {
        task.iter_path_inputs()
            .any(|input_glob| matches_any(changed_files, project_root, input_glob))
    } else {
        false
    }
}

#[allow(clippy::implicit_hasher)]
fn check_external_dependency(
    dep: &str,
    all_projects: &HashMap<String, DiscoveredCIProject>,
    changed_files: &[PathBuf],
    cache: &mut HashMap<String, bool>,
) -> bool {
    // dep format: "#project:task"
    if let Some(result) = cache.get(dep) {
        return *result;
    }

    // Break recursion cycle by assuming false initially (or handle cycles better?)
    // For DAGs, temporary false is okay.
    cache.insert(dep.to_string(), false);

    let parts: Vec<&str> = dep[1..].split(':').collect();
    if parts.len() < 2 {
        return false;
    }
    let project_name = parts[0];
    let task_name = parts[1];

    let Some(project) = all_projects.get(project_name) else {
        return false;
    };

    let project_root = project.path.parent().unwrap_or_else(|| Path::new("."));

    // Check if directly affected
    if is_task_directly_affected(task_name, &project.config, changed_files, project_root) {
        cache.insert(dep.to_string(), true);
        return true;
    }

    // Check transitive dependencies of the external task
    if let Some(task_def) = project.config.tasks.get(task_name)
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

fn matches_any(files: &[PathBuf], root: &Path, pattern: &str) -> bool {
    // If pattern doesn't contain glob characters, treat it as a path prefix
    // e.g., "crates" should match "crates/foo/bar.rs"
    let is_simple_path = !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('[');

    for file in files {
        // Get relative path - if root is "." or empty, use file as-is
        // Otherwise strip the prefix
        let relative_path = if root == Path::new(".") || root.as_os_str().is_empty() {
            file.as_path()
        } else {
            match file.strip_prefix(root) {
                Ok(p) => p,
                Err(_) => continue,
            }
        };

        if is_simple_path {
            // Check if the pattern is a prefix of the file path or exact match
            let pattern_path = Path::new(pattern);
            if relative_path.starts_with(pattern_path) || relative_path == pattern_path {
                return true;
            }
        } else {
            // Use glob matching for patterns with wildcards
            let Ok(glob) = glob::Pattern::new(pattern) else {
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
    use cuenv_core::tasks::{Input, Task, TaskDefinition};

    fn create_test_task(inputs: Vec<String>, depends_on: Vec<String>) -> Task {
        Task {
            shell: None,
            command: "echo".to_string(),
            script: None,
            args: vec!["test".to_string()],
            env: std::collections::HashMap::new(),
            dagger: None,
            hermetic: true,
            depends_on,
            inputs: inputs.into_iter().map(Input::Path).collect(),
            outputs: vec![],
            inputs_from: None,
            workspaces: vec![],
            description: None,
            params: None,
            labels: vec![],
            task_ref: None,
            project_root: None,
            source: None,
            directory: None,
        }
    }

    fn create_test_project(tasks: Vec<(&str, Task)>) -> Project {
        let mut task_map = std::collections::HashMap::new();
        for (name, task) in tasks {
            task_map.insert(name.to_string(), TaskDefinition::Single(Box::new(task)));
        }
        Project {
            config: None,
            name: "test-project".to_string(),
            env: None,
            hooks: None,
            workspaces: None,
            ci: None,
            owners: None,
            tasks: task_map,
            ignore: None,
            cube: None,
        }
    }

    // ===== matches_any tests =====

    #[test]
    fn test_matches_any_exact_path() {
        let files = vec![PathBuf::from("src/main.rs")];
        assert!(matches_any(&files, Path::new("."), "src/main.rs"));
    }

    #[test]
    fn test_matches_any_prefix_path() {
        let files = vec![PathBuf::from("src/lib/utils.rs")];
        assert!(matches_any(&files, Path::new("."), "src"));
    }

    #[test]
    fn test_matches_any_glob_wildcard() {
        let files = vec![PathBuf::from("src/main.rs")];
        assert!(matches_any(&files, Path::new("."), "src/*.rs"));
    }

    #[test]
    fn test_matches_any_glob_double_star() {
        let files = vec![PathBuf::from("src/nested/deep/file.rs")];
        assert!(matches_any(&files, Path::new("."), "src/**/*.rs"));
    }

    #[test]
    fn test_matches_any_no_match() {
        let files = vec![PathBuf::from("src/main.rs")];
        assert!(!matches_any(&files, Path::new("."), "tests"));
    }

    #[test]
    fn test_matches_any_with_project_root() {
        let files = vec![PathBuf::from("/project/src/main.rs")];
        assert!(matches_any(&files, Path::new("/project"), "src/main.rs"));
    }

    #[test]
    fn test_matches_any_file_outside_root() {
        let files = vec![PathBuf::from("/other/src/main.rs")];
        assert!(!matches_any(&files, Path::new("/project"), "src"));
    }

    // ===== compute_affected_tasks tests =====

    #[test]
    fn test_compute_affected_simple_match() {
        let task = create_test_task(vec!["src/**/*.rs".to_string()], vec![]);
        let project = create_test_project(vec![("build", task)]);
        let changed_files = vec![PathBuf::from("src/main.rs")];
        let pipeline_tasks = vec!["build".to_string()];

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            Path::new("."),
            &project,
            &HashMap::new(),
        );

        assert_eq!(affected, vec!["build"]);
    }

    #[test]
    fn test_compute_affected_no_match() {
        let task = create_test_task(vec!["src/**/*.rs".to_string()], vec![]);
        let project = create_test_project(vec![("build", task)]);
        let changed_files = vec![PathBuf::from("docs/README.md")];
        let pipeline_tasks = vec!["build".to_string()];

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            Path::new("."),
            &project,
            &HashMap::new(),
        );

        assert!(affected.is_empty());
    }

    #[test]
    fn test_compute_affected_transitive_dependency() {
        let build_task = create_test_task(vec!["src/**/*.rs".to_string()], vec![]);
        let test_task = create_test_task(vec![], vec!["build".to_string()]);
        let project = create_test_project(vec![("build", build_task), ("test", test_task)]);
        let changed_files = vec![PathBuf::from("src/main.rs")];
        let pipeline_tasks = vec!["build".to_string(), "test".to_string()];

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            Path::new("."),
            &project,
            &HashMap::new(),
        );

        assert_eq!(affected, vec!["build", "test"]);
    }

    #[test]
    fn test_compute_affected_preserves_pipeline_order() {
        let task_a = create_test_task(vec!["src/a.rs".to_string()], vec![]);
        let task_b = create_test_task(vec!["src/b.rs".to_string()], vec![]);
        let task_c = create_test_task(vec!["src/c.rs".to_string()], vec![]);
        let project = create_test_project(vec![("a", task_a), ("b", task_b), ("c", task_c)]);
        let changed_files = vec![
            PathBuf::from("src/c.rs"),
            PathBuf::from("src/a.rs"),
            PathBuf::from("src/b.rs"),
        ];
        let pipeline_tasks = vec!["a".to_string(), "b".to_string(), "c".to_string()];

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            Path::new("."),
            &project,
            &HashMap::new(),
        );

        // Should preserve pipeline order, not file order
        assert_eq!(affected, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_compute_affected_chain_dependency() {
        let task_a = create_test_task(vec!["src/**/*.rs".to_string()], vec![]);
        let task_b = create_test_task(vec![], vec!["a".to_string()]);
        let task_c = create_test_task(vec![], vec!["b".to_string()]);
        let project = create_test_project(vec![("a", task_a), ("b", task_b), ("c", task_c)]);
        let changed_files = vec![PathBuf::from("src/main.rs")];
        let pipeline_tasks = vec!["a".to_string(), "b".to_string(), "c".to_string()];

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            Path::new("."),
            &project,
            &HashMap::new(),
        );

        assert_eq!(affected, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_compute_affected_missing_task_not_in_project() {
        let project = create_test_project(vec![]);
        let changed_files = vec![PathBuf::from("src/main.rs")];
        let pipeline_tasks = vec!["nonexistent".to_string()];

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            Path::new("."),
            &project,
            &HashMap::new(),
        );

        assert!(affected.is_empty());
    }

    // ===== matched_inputs_for_task tests =====

    #[test]
    fn test_matched_inputs_for_task_single_match() {
        let task = create_test_task(vec!["src/**/*.rs".to_string()], vec![]);
        let project = create_test_project(vec![("build", task)]);
        let changed_files = vec![PathBuf::from("src/main.rs")];

        let matched = matched_inputs_for_task("build", &project, &changed_files, Path::new("."));

        assert_eq!(matched, vec!["src/**/*.rs"]);
    }

    #[test]
    fn test_matched_inputs_for_task_multiple_inputs() {
        let task = create_test_task(
            vec![
                "src/**/*.rs".to_string(),
                "tests/**/*.rs".to_string(),
                "docs/**/*.md".to_string(),
            ],
            vec![],
        );
        let project = create_test_project(vec![("build", task)]);
        let changed_files = vec![
            PathBuf::from("src/main.rs"),
            PathBuf::from("tests/test.rs"),
        ];

        let matched = matched_inputs_for_task("build", &project, &changed_files, Path::new("."));

        assert!(matched.contains(&"src/**/*.rs".to_string()));
        assert!(matched.contains(&"tests/**/*.rs".to_string()));
        assert!(!matched.contains(&"docs/**/*.md".to_string()));
    }

    #[test]
    fn test_matched_inputs_for_task_nonexistent() {
        let project = create_test_project(vec![]);
        let changed_files = vec![PathBuf::from("src/main.rs")];

        let matched =
            matched_inputs_for_task("nonexistent", &project, &changed_files, Path::new("."));

        assert!(matched.is_empty());
    }

    // ===== Cross-project dependency tests =====

    #[test]
    fn test_compute_affected_external_dependency() {
        // Project A has a task that depends on external project B
        let task_in_a = create_test_task(vec![], vec!["#project-b:build".to_string()]);
        let project_a = create_test_project(vec![("deploy", task_in_a)]);

        // Project B has a build task with inputs
        let task_in_b = create_test_task(vec!["src/**/*.rs".to_string()], vec![]);
        let project_b = create_test_project(vec![("build", task_in_b)]);

        let mut all_projects = HashMap::new();
        all_projects.insert(
            "project-b".to_string(),
            DiscoveredCIProject {
                path: PathBuf::from("packages/project-b/env.cue"),
                config: project_b,
            },
        );

        // Change in project B should affect project A's deploy task
        let changed_files = vec![PathBuf::from("packages/project-b/src/lib.rs")];
        let pipeline_tasks = vec!["deploy".to_string()];

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            Path::new("packages/project-a"),
            &project_a,
            &all_projects,
        );

        assert_eq!(affected, vec!["deploy"]);
    }

    #[test]
    fn test_compute_affected_external_dependency_no_match() {
        let task_in_a = create_test_task(vec![], vec!["#project-b:build".to_string()]);
        let project_a = create_test_project(vec![("deploy", task_in_a)]);

        let task_in_b = create_test_task(vec!["src/**/*.rs".to_string()], vec![]);
        let project_b = create_test_project(vec![("build", task_in_b)]);

        let mut all_projects = HashMap::new();
        all_projects.insert(
            "project-b".to_string(),
            DiscoveredCIProject {
                path: PathBuf::from("packages/project-b/env.cue"),
                config: project_b,
            },
        );

        // Change in unrelated location should not affect
        let changed_files = vec![PathBuf::from("packages/project-c/src/lib.rs")];
        let pipeline_tasks = vec!["deploy".to_string()];

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            Path::new("packages/project-a"),
            &project_a,
            &all_projects,
        );

        assert!(affected.is_empty());
    }

    #[test]
    fn test_compute_affected_missing_external_project() {
        let task_in_a = create_test_task(vec![], vec!["#nonexistent:build".to_string()]);
        let project_a = create_test_project(vec![("deploy", task_in_a)]);

        let changed_files = vec![PathBuf::from("src/main.rs")];
        let pipeline_tasks = vec!["deploy".to_string()];

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            Path::new("."),
            &project_a,
            &HashMap::new(),
        );

        // Should not crash, just not match
        assert!(affected.is_empty());
    }

    #[test]
    fn test_compute_affected_malformed_external_ref() {
        // Test that malformed references (missing colon) don't crash
        let task = create_test_task(vec![], vec!["#invalid".to_string()]);
        let project = create_test_project(vec![("task", task)]);

        let changed_files = vec![PathBuf::from("src/main.rs")];
        let pipeline_tasks = vec!["task".to_string()];

        let affected = compute_affected_tasks(
            &changed_files,
            &pipeline_tasks,
            Path::new("."),
            &project,
            &HashMap::new(),
        );

        assert!(affected.is_empty());
    }
}
