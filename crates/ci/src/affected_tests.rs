use super::{AffectedProjects, ChangedInputs, ExternalDependencyContext, HashMap, Path, PathBuf};
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

    let matched = super::matched_inputs_for_task(
        "build",
        &project,
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
    );

    assert_eq!(matched, vec!["src/**".to_string()]);
}

#[test]
fn test_matched_inputs_for_task_no_match() {
    let task = make_task(vec!["src/**"], vec![]);
    let project = make_project(vec![("build", task)]);
    let changed_files = vec![PathBuf::from("tests/test.rs")];
    let root = Path::new(".");

    let matched = super::matched_inputs_for_task(
        "build",
        &project,
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
    );

    assert!(matched.is_empty());
}

#[test]
fn test_matched_inputs_for_task_nonexistent_task() {
    let project = Project::default();
    let changed_files = vec![PathBuf::from("src/lib.rs")];
    let root = Path::new(".");

    let matched = super::matched_inputs_for_task(
        "nonexistent",
        &project,
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
    );

    assert!(matched.is_empty());
}

#[test]
fn test_matched_inputs_for_task_multiple_matches() {
    let task = make_task(vec!["src/**", "lib/**", "Cargo.toml"], vec![]);
    let project = make_project(vec![("build", task)]);
    let changed_files = vec![PathBuf::from("src/lib.rs"), PathBuf::from("lib/util.rs")];
    let root = Path::new(".");

    let matched = super::matched_inputs_for_task(
        "build",
        &project,
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
    );

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

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
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

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
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

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
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

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
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

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
    );

    // Only build should be returned (test not in pipeline)
    assert_eq!(affected, vec!["build"]);
}

#[test]
fn test_compute_affected_tasks_deploy_pipeline_with_absolute_project_root() {
    let generate_types = make_task(
        vec![
            "../package.json",
            "../bun.lock",
            "package.json",
            "wrangler.jsonc",
        ],
        vec![],
    );
    let build = make_task(
        vec![
            "../package.json",
            "../bun.lock",
            "package.json",
            "astro.config.mjs",
            "tsconfig.json",
            "wrangler.jsonc",
            "src/**",
            "public/**",
        ],
        vec!["generateTypes"],
    );
    let deploy = make_task(vec!["wrangler.jsonc", "dist/**"], vec!["build"]);

    let project = make_project(vec![
        ("generateTypes", generate_types),
        ("build", build),
        ("deploy", deploy),
    ]);
    let changed_files = vec![PathBuf::from("chat/src/components/chat/ChatEditor.vue")];
    let root = Path::new("chat");
    let pipeline_tasks = vec!["deploy".to_string()];
    let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
    );

    assert_eq!(affected, vec!["deploy"]);
}

#[test]
fn test_matched_inputs_for_task_with_absolute_project_root() {
    let task = make_task(vec!["src/**", "public/**"], vec![]);
    let project = make_project(vec![("build", task)]);
    let changed_files = vec![PathBuf::from("chat/src/components/chat/ChatEditor.vue")];
    let root = Path::new("chat");

    let matched = super::matched_inputs_for_task(
        "build",
        &project,
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
    );

    assert_eq!(matched, vec!["src/**".to_string()]);
}

#[test]
fn test_compute_affected_tasks_empty_pipeline() {
    let task = make_task(vec!["src/**"], vec![]);
    let project = make_project(vec![("build", task)]);
    let changed_files = vec![PathBuf::from("src/lib.rs")];
    let root = Path::new(".");
    let pipeline_tasks: Vec<String> = vec![];
    let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
    );

    assert!(affected.is_empty());
}

#[test]
fn test_compute_affected_tasks_external_dep_not_found() {
    // An external dependency on a project that is not in the module cannot be
    // shown to be unaffected, so the task is selected (fail closed).
    // Task has inputs so it won't be auto-affected
    let task = make_task(vec!["deploy/**"], vec!["#nonexistent:build"]);
    let project = make_project(vec![("deploy", task)]);
    let changed_files = vec![PathBuf::from("src/lib.rs")]; // Doesn't match deploy/**
    let root = Path::new(".");
    let pipeline_tasks = vec!["deploy".to_string()];
    let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
    );

    assert_eq!(affected, vec!["deploy".to_string()]);
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
        (PathBuf::from("external"), external_project),
    );

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
    );

    assert!(affected.contains(&"deploy".to_string()));
}

#[test]
fn test_compute_affected_tasks_external_nested_dep_affected() {
    let external_preview = make_task(vec!["src/**"], vec![]);
    let external_project = make_project(vec![("deploy.preview", external_preview)]);

    let deploy_task = make_task(vec!["deploy/**"], vec!["#external:deploy:preview"]);
    let project = make_project(vec![("deploy", deploy_task)]);

    let changed_files = vec![PathBuf::from("src/lib.rs")];
    let root = Path::new(".");
    let pipeline_tasks = vec!["deploy".to_string()];

    let mut all_projects = HashMap::new();
    all_projects.insert(
        "external".to_string(),
        (PathBuf::from("external"), external_project),
    );

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
    );

    assert_eq!(affected, vec!["deploy".to_string()]);
}

#[test]
fn test_compute_affected_tasks_malformed_external_dep() {
    // A malformed external dependency (missing colon) cannot be shown to be
    // unaffected, so the task is selected (fail closed).
    // Task has inputs so it won't be auto-affected
    let task = make_task(vec!["deploy/**"], vec!["#badformat"]);
    let project = make_project(vec![("deploy", task)]);
    let changed_files = vec![PathBuf::from("src/lib.rs")]; // Doesn't match deploy/**
    let root = Path::new(".");
    let pipeline_tasks = vec!["deploy".to_string()];
    let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
    );

    assert_eq!(affected, vec!["deploy".to_string()]);
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

    assert!(super::is_task_directly_affected(
        "build",
        &project,
        ChangedInputs {
            files: &changed_files,
            project_root: root
        }
    ));
}

#[test]
fn test_is_task_directly_affected_no_match() {
    let task = make_task(vec!["src/**"], vec![]);
    let project = make_project(vec![("build", task)]);
    let changed_files = vec![PathBuf::from("docs/readme.md")];
    let root = Path::new(".");

    assert!(!super::is_task_directly_affected(
        "build",
        &project,
        ChangedInputs {
            files: &changed_files,
            project_root: root
        }
    ));
}

#[test]
fn test_is_task_directly_affected_nonexistent_task() {
    let project = Project::default();
    let changed_files = vec![PathBuf::from("src/lib.rs")];
    let root = Path::new(".");

    assert!(!super::is_task_directly_affected(
        "nonexistent",
        &project,
        ChangedInputs {
            files: &changed_files,
            project_root: root
        }
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

    assert!(super::is_task_directly_affected(
        "build",
        &project,
        ChangedInputs {
            files: &changed_files,
            project_root: root
        }
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

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
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

    assert!(super::is_task_directly_affected(
        "check",
        &project,
        ChangedInputs {
            files: &changed_files,
            project_root: root
        }
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

    assert!(!super::is_task_directly_affected(
        "check",
        &project,
        ChangedInputs {
            files: &changed_files,
            project_root: root
        }
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

    assert!(super::is_task_directly_affected(
        "check",
        &project,
        ChangedInputs {
            files: &changed_files,
            project_root: root
        }
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

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
    );

    // "check" should be in affected list because its "lint" subtask is affected
    assert_eq!(affected, vec!["check".to_string()]);
}

// ==========================================================================
// check_external_dependency tests
// ==========================================================================

struct ExternalTestInputs<'fixture, S> {
    projects: &'fixture HashMap<String, (PathBuf, Project), S>,
    changed_files: &'fixture [PathBuf],
    cache: &'fixture mut HashMap<String, bool>,
}

fn check_external_dependency_for_test<S>(dep: &str, inputs: &mut ExternalTestInputs<'_, S>) -> bool
where
    S: std::hash::BuildHasher,
{
    let mut index_cache = HashMap::new();
    let mut visiting = std::collections::HashSet::new();
    super::check_external_dependency(
        dep,
        &mut ExternalDependencyContext {
            projects: inputs.projects,
            checkout_root: None,
            changed_configuration: None,
            changed_files: inputs.changed_files,
            cache: inputs.cache,
            visiting: &mut visiting,
            index_cache: &mut index_cache,
        },
    )
}

#[test]
fn test_check_external_dependency_cache_hit() {
    let mut cache = HashMap::new();
    cache.insert("#project:task".to_string(), true);
    let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();
    let changed_files: Vec<PathBuf> = vec![];

    let result = check_external_dependency_for_test(
        "#project:task",
        &mut ExternalTestInputs {
            projects: &all_projects,
            changed_files: &changed_files,
            cache: &mut cache,
        },
    );

    assert!(result);
}

#[test]
fn test_check_external_dependency_cache_miss_false() {
    let mut cache = HashMap::new();
    cache.insert("#project:task".to_string(), false);
    let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();
    let changed_files: Vec<PathBuf> = vec![];

    let result = check_external_dependency_for_test(
        "#project:task",
        &mut ExternalTestInputs {
            projects: &all_projects,
            changed_files: &changed_files,
            cache: &mut cache,
        },
    );

    assert!(!result);
}

#[test]
fn test_check_external_dependency_project_not_found() {
    let mut cache = HashMap::new();
    let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();
    let changed_files = vec![PathBuf::from("src/lib.rs")];

    let result = check_external_dependency_for_test(
        "#missing:task",
        &mut ExternalTestInputs {
            projects: &all_projects,
            changed_files: &changed_files,
            cache: &mut cache,
        },
    );

    // Fail closed: an unknown project cannot be shown to be unaffected.
    assert!(result);
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
        (PathBuf::from("external"), external_project),
    );

    let changed_files = vec![PathBuf::from("src/lib.rs")];
    let mut cache = HashMap::new();

    let result = check_external_dependency_for_test(
        "#external:build",
        &mut ExternalTestInputs {
            projects: &all_projects,
            changed_files: &changed_files,
            cache: &mut cache,
        },
    );

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
        (PathBuf::from("external"), external_project),
    );

    let changed_files = vec![PathBuf::from("src/lib.rs")];
    let mut cache = HashMap::new();

    let result = check_external_dependency_for_test(
        "#external:test",
        &mut ExternalTestInputs {
            projects: &all_projects,
            changed_files: &changed_files,
            cache: &mut cache,
        },
    );

    assert!(result);
}

#[test]
fn test_check_external_dependency_circular_prevention() {
    // Task A depends on itself (circular): the check terminates, and a cycle
    // counts as affected because nothing shows it is not.
    // Task has inputs so it won't be auto-affected
    let circular_task = make_task(vec!["taskA/**"], vec!["#proj:taskA"]);
    let mut project = Project::default();
    project
        .tasks
        .insert("taskA".to_string(), TaskNode::Task(Box::new(circular_task)));

    let mut all_projects = HashMap::new();
    all_projects.insert("proj".to_string(), (PathBuf::from("proj"), project));

    let changed_files: Vec<PathBuf> = vec![]; // No changes matching taskA/**
    let mut cache = HashMap::new();

    let result = check_external_dependency_for_test(
        "#proj:taskA",
        &mut ExternalTestInputs {
            projects: &all_projects,
            changed_files: &changed_files,
            cache: &mut cache,
        },
    );

    assert!(result);
}

// ==========================================================================
// DAG expansion and non-pipeline dependency tests
// ==========================================================================

#[test]
fn test_compute_affected_tasks_non_pipeline_dep() {
    // Pipeline only lists deploy, but deploy depends on build.
    // build's inputs match changed files -> deploy should be affected.
    let build_task = make_task(vec!["src/**"], vec![]);
    let deploy_task = make_task(vec!["dist/**"], vec!["build"]);
    let project = make_project(vec![("build", build_task), ("deploy", deploy_task)]);
    let changed_files = vec![PathBuf::from("src/app.vue")];
    let root = Path::new(".");
    let pipeline_tasks = vec!["deploy".to_string()];
    let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
    );

    assert_eq!(
        affected,
        vec!["deploy"],
        "deploy should be affected via its non-pipeline build dependency"
    );
}

#[test]
fn test_compute_affected_tasks_deep_non_pipeline_chain() {
    // Pipeline: [deploy], DAG: deploy -> build -> generateTypes
    // Only generateTypes inputs match -> deploy should still be affected.
    let gen_task = make_task(vec!["schema/**"], vec![]);
    let build_task = make_task(vec!["dist/**"], vec!["generateTypes"]);
    let deploy_task = make_task(vec!["release/**"], vec!["build"]);
    let project = make_project(vec![
        ("generateTypes", gen_task),
        ("build", build_task),
        ("deploy", deploy_task),
    ]);
    let changed_files = vec![PathBuf::from("schema/types.graphql")];
    let root = Path::new(".");
    let pipeline_tasks = vec!["deploy".to_string()];
    let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
    );

    assert_eq!(
        affected,
        vec!["deploy"],
        "deploy should be affected via generateTypes -> build -> deploy chain"
    );
}

#[test]
fn test_compute_affected_tasks_non_pipeline_dep_no_match() {
    // Pipeline: [deploy], DAG: deploy -> build
    // Neither deploy nor build inputs match -> deploy should NOT be affected.
    let build_task = make_task(vec!["src/**"], vec![]);
    let deploy_task = make_task(vec!["dist/**"], vec!["build"]);
    let project = make_project(vec![("build", build_task), ("deploy", deploy_task)]);
    let changed_files = vec![PathBuf::from("docs/readme.md")];
    let root = Path::new(".");
    let pipeline_tasks = vec!["deploy".to_string()];
    let all_projects: HashMap<String, (PathBuf, Project)> = HashMap::new();

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed_files,
            project_root: root,
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: &project,
            all_projects: &all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
    );

    assert!(
        affected.is_empty(),
        "deploy should not be affected when no DAG inputs match"
    );
}

// ==========================================================================
// Fail-closed selection (issue 611)
// ==========================================================================

/// A change in a module, as seen from the project `/repo/app`.
struct Change<'change> {
    project: &'change Project,
    all_projects: &'change HashMap<String, (PathBuf, Project)>,
    files: &'change [PathBuf],
}

fn affected_in(change: &Change<'_>, pipeline_tasks: &[&str]) -> Vec<String> {
    let pipeline_tasks: Vec<String> = pipeline_tasks.iter().map(ToString::to_string).collect();
    super::compute_affected_tasks(
        ChangedInputs {
            files: change.files,
            project_root: Path::new("app"),
        },
        &pipeline_tasks,
        &AffectedProjects {
            project: change.project,
            all_projects: change.all_projects,
            checkout_root: None,
            changed_configuration: None,
        },
    )
}

fn content_project() -> HashMap<String, (PathBuf, Project)> {
    let articles = make_task(vec!["articles/**"], vec![]);
    let mut all_projects = HashMap::new();
    all_projects.insert(
        "content".to_string(),
        (
            PathBuf::from("content"),
            make_project(vec![("articles", articles)]),
        ),
    );
    all_projects
}

fn group_of(children: Vec<(&str, TaskNode)>) -> TaskNode {
    TaskNode::Group(TaskGroup {
        type_: "group".to_string(),
        depends_on: vec![],
        max_concurrency: None,
        description: None,
        children: children
            .into_iter()
            .map(|(name, node)| (name.to_string(), node))
            .collect(),
    })
}

#[test]
fn a_group_child_with_an_external_dependency_selects_the_group() {
    let child = make_task(vec!["src/**"], vec!["#content:articles"]);
    let mut project = Project::default();
    project.tasks.insert(
        "check".to_string(),
        group_of(vec![("site", TaskNode::Task(Box::new(child)))]),
    );
    let changed = vec![PathBuf::from("content/articles/one.md")];

    assert_eq!(
        affected_in(
            &Change {
                project: &project,
                all_projects: &content_project(),
                files: &changed
            },
            &["check"]
        ),
        ["check"]
    );
    assert!(
        affected_in(
            &Change {
                project: &project,
                all_projects: &content_project(),
                files: &[PathBuf::from("content/other.md")]
            },
            &["check"]
        )
        .is_empty()
    );
}

#[test]
fn a_sequence_step_with_an_external_dependency_selects_the_sequence() {
    let first = make_task(vec!["src/**"], vec![]);
    let second = make_task(vec!["src/**"], vec!["#content:articles"]);
    let mut project = Project::default();
    project.tasks.insert(
        "release".to_string(),
        TaskNode::Sequence(vec![
            TaskNode::Task(Box::new(first)),
            TaskNode::Task(Box::new(second)),
        ]),
    );
    let changed = vec![PathBuf::from("content/articles/one.md")];

    assert_eq!(
        affected_in(
            &Change {
                project: &project,
                all_projects: &content_project(),
                files: &changed
            },
            &["release"]
        ),
        ["release"]
    );
}

#[test]
fn a_group_dependency_of_a_group_child_is_followed() {
    // build depends on a local group whose child reads the changed file.
    let generator = make_task(vec!["schema/**"], vec![]);
    let build = make_task(vec!["src/**"], vec!["generate"]);
    let mut project = Project::default();
    project.tasks.insert(
        "generate".to_string(),
        group_of(vec![("types", TaskNode::Task(Box::new(generator)))]),
    );
    project.tasks.insert(
        "ci".to_string(),
        group_of(vec![("build", TaskNode::Task(Box::new(build)))]),
    );
    let changed = vec![PathBuf::from("app/schema/user.cue")];

    assert_eq!(
        affected_in(
            &Change {
                project: &project,
                all_projects: &HashMap::new(),
                files: &changed
            },
            &["ci"]
        ),
        ["ci"]
    );
}

#[test]
fn an_external_producer_that_is_a_group_is_expanded_to_its_tasks() {
    let consumer = make_task(vec!["src/**"], vec!["#content:build"]);
    let project = make_project(vec![("build", consumer)]);
    let inner = make_task(vec!["site/**"], vec!["articles"]);
    let articles = make_task(vec!["articles/**"], vec![]);
    let mut content = make_project(vec![("articles", articles)]);
    content.tasks.insert(
        "build".to_string(),
        group_of(vec![("site", TaskNode::Task(Box::new(inner)))]),
    );
    let mut all_projects = HashMap::new();
    all_projects.insert("content".to_string(), (PathBuf::from("content"), content));
    let changed = vec![PathBuf::from("content/articles/one.md")];

    assert_eq!(
        affected_in(
            &Change {
                project: &project,
                all_projects: &all_projects,
                files: &changed
            },
            &["build"]
        ),
        ["build"]
    );
}

#[test]
fn a_task_the_external_project_does_not_have_counts_as_affected() {
    let consumer = make_task(vec!["src/**"], vec!["#content:missing"]);
    let project = make_project(vec![("build", consumer)]);
    let changed = vec![PathBuf::from("docs/readme.md")];

    assert_eq!(
        affected_in(
            &Change {
                project: &project,
                all_projects: &content_project(),
                files: &changed
            },
            &["build"]
        ),
        ["build"]
    );
}

#[test]
fn a_pipeline_task_that_does_not_resolve_is_selected() {
    let build = make_task(vec!["src/**"], vec![]);
    let project = make_project(vec![("build", build)]);
    let changed = vec![PathBuf::from("docs/readme.md")];

    assert_eq!(
        affected_in(
            &Change {
                project: &project,
                all_projects: &HashMap::new(),
                files: &changed
            },
            &["build", "typo"]
        ),
        ["typo"]
    );
}

#[test]
fn a_parent_directory_input_selects_its_task_whenever_anything_changed() {
    // Issue 611, case 2: the only changed file is outside the project.
    let build = make_task(vec!["src/**", "../content/articles/**"], vec![]);
    let project = make_project(vec![("build", build)]);
    let changed = vec![PathBuf::from("content/articles/one.md")];

    assert_eq!(
        affected_in(
            &Change {
                project: &project,
                all_projects: &HashMap::new(),
                files: &changed
            },
            &["build"]
        ),
        ["build"]
    );
    assert!(
        affected_in(
            &Change {
                project: &project,
                all_projects: &HashMap::new(),
                files: &[]
            },
            &["build"]
        )
        .is_empty()
    );
    assert_eq!(
        super::matched_inputs_for_task(
            "build",
            &project,
            ChangedInputs {
                files: &changed,
                project_root: Path::new("app"),
            },
        ),
        ["../content/articles/**"]
    );
}

#[test]
fn a_group_child_with_a_parent_directory_input_selects_the_group() {
    let child = make_task(vec!["../shared/**"], vec![]);
    let mut project = Project::default();
    project.tasks.insert(
        "check".to_string(),
        group_of(vec![("lint", TaskNode::Task(Box::new(child)))]),
    );
    let changed = vec![PathBuf::from("docs/readme.md")];

    assert_eq!(
        affected_in(
            &Change {
                project: &project,
                all_projects: &HashMap::new(),
                files: &changed
            },
            &["check"]
        ),
        ["check"]
    );
}

#[test]
fn an_absolute_input_selects_its_task_whenever_anything_changed() {
    let build = make_task(vec!["/etc/hosts"], vec![]);
    let project = make_project(vec![("build", build)]);
    let changed = vec![PathBuf::from("docs/readme.md")];

    assert_eq!(
        affected_in(
            &Change {
                project: &project,
                all_projects: &HashMap::new(),
                files: &changed
            },
            &["build"]
        ),
        ["build"]
    );
}

// ==========================================================================
// One path frame (issue 611, A.8)
// ==========================================================================

fn checkout_with(files: &[&str]) -> tempfile::TempDir {
    let checkout = tempfile::tempdir().unwrap();
    for file in files {
        let path = checkout.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "content").unwrap();
    }
    checkout
}

#[test]
fn a_checkout_frame_places_a_project_by_an_exact_prefix() {
    let checkout = checkout_with(&["docs/index.md", "apps/web/src/main.ts"]);
    let frame = super::CheckoutFrame::at(checkout.path());

    assert_eq!(
        frame.place(&checkout.path().join("apps/web")),
        Some(PathBuf::from("apps/web"))
    );
    assert_eq!(frame.place(checkout.path()), Some(PathBuf::new()));
    assert_eq!(frame.place(Path::new("/somewhere/else")), None);
    assert_eq!(
        frame.place(&checkout.path().join("apps/web/../../docs")),
        Some(PathBuf::from("docs"))
    );
}

#[test]
fn an_input_read_only_from_the_checkout_root_keeps_its_task_affected_and_is_reported() {
    // `app` reads `shared/**`, and the only `shared/` is at the root.
    let checkout = checkout_with(&["app/src/main.ts", "shared/x.txt"]);
    let build = make_task(vec!["src/**", "shared/**"], vec![]);
    let project = make_project(vec![("build", build)]);
    let changed = vec![PathBuf::from("shared/x.txt")];
    let inputs = ChangedInputs {
        files: &changed,
        project_root: Path::new("app"),
    };

    let affected = super::compute_affected_tasks(
        inputs,
        &["build".to_string()],
        &AffectedProjects {
            project: &project,
            all_projects: &HashMap::new(),
            checkout_root: Some(checkout.path()),
            changed_configuration: None,
        },
    );

    assert_eq!(affected, ["build"]);
    assert_eq!(
        super::root_relative_inputs(inputs, checkout.path(), &project.tasks["build"]),
        ["shared/**"]
    );
}

#[test]
fn a_correct_project_whose_root_has_files_of_the_same_name_is_not_reported() {
    // The ordinary monorepo: `app` has its own `package.json` and `src/`,
    // and so does the root. A root change still selects the task for now,
    // and the configuration is not at fault.
    let checkout = checkout_with(&[
        "app/package.json",
        "app/src/main.ts",
        "package.json",
        "src/root.ts",
    ]);
    let build = make_task(vec!["package.json", "src/**"], vec![]);
    let project = make_project(vec![("build", build)]);

    for changed in [
        vec![PathBuf::from("package.json")],
        vec![PathBuf::from("src/root.ts")],
    ] {
        let inputs = ChangedInputs {
            files: &changed,
            project_root: Path::new("app"),
        };
        let affected = super::compute_affected_tasks(
            inputs,
            &["build".to_string()],
            &AffectedProjects {
                project: &project,
                all_projects: &HashMap::new(),
                checkout_root: Some(checkout.path()),
                changed_configuration: None,
            },
        );

        assert_eq!(affected, ["build"]);
        assert!(
            super::root_relative_inputs(inputs, checkout.path(), &project.tasks["build"])
                .is_empty()
        );
    }
}

#[test]
fn a_checkout_named_like_one_of_its_projects_still_matches() {
    // `.../docs/docs`: the project `docs` in a checkout directory `docs`.
    let outer = tempfile::tempdir().unwrap();
    let checkout = outer.path().join("docs");
    std::fs::create_dir_all(checkout.join("docs")).unwrap();
    std::fs::write(checkout.join("docs/index.md"), "content").unwrap();
    let frame = super::CheckoutFrame::at(&checkout);
    let project_root = frame.place(&checkout.join("docs")).unwrap();
    let lint = make_task(vec!["**/*.md"], vec![]);
    let project = make_project(vec![("lint", lint)]);
    let changed = vec![PathBuf::from("docs/index.md")];

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed,
            project_root: &project_root,
        },
        &["lint".to_string()],
        &AffectedProjects {
            project: &project,
            all_projects: &HashMap::new(),
            checkout_root: Some(frame.root()),
            changed_configuration: None,
        },
    );

    assert_eq!(project_root, PathBuf::from("docs"));
    assert_eq!(affected, ["lint"]);
}

// ==========================================================================
// CUE-only changes (issue 611, PR-H)
// ==========================================================================

#[test]
fn a_project_whose_configuration_changed_has_every_pipeline_task_selected() {
    let build = make_task(vec!["src/**"], vec![]);
    let test = make_task(vec!["tests/**"], vec![]);
    let project = make_project(vec![("build", build), ("test", test)]);
    let changed = vec![PathBuf::from("app/env.cue")];
    let changed_configuration = std::collections::HashSet::from([PathBuf::from("app")]);
    let pipeline = ["build".to_string(), "test".to_string()];

    let select = |changed_configuration| {
        super::compute_affected_tasks(
            ChangedInputs {
                files: &changed,
                project_root: Path::new("app"),
            },
            &pipeline,
            &AffectedProjects {
                project: &project,
                all_projects: &HashMap::new(),
                checkout_root: None,
                changed_configuration,
            },
        )
    };

    assert_eq!(select(Some(&changed_configuration)), ["build", "test"]);
    // The `.cue` file is not a declared input of either task.
    assert!(select(Some(&std::collections::HashSet::new())).is_empty());
}

#[test]
fn a_root_project_whose_configuration_changed_is_found_under_either_spelling_of_its_root() {
    let build = make_task(vec!["src/**"], vec![]);
    let project = make_project(vec![("build", build)]);
    let changed = vec![PathBuf::from("env.cue")];
    let changed_configuration = std::collections::HashSet::from([PathBuf::new()]);

    for root in [".", ""] {
        let affected = super::compute_affected_tasks(
            ChangedInputs {
                files: &changed,
                project_root: Path::new(root),
            },
            &["build".to_string()],
            &AffectedProjects {
                project: &project,
                all_projects: &HashMap::new(),
                checkout_root: None,
                changed_configuration: Some(&changed_configuration),
            },
        );
        assert_eq!(affected, ["build"], "{root:?}");
    }
}

#[test]
fn a_producer_whose_configuration_changed_affects_its_consumers() {
    // `app` names `content` by `#content:articles`, so it does not import it
    // and `content/env.cue` is not among `app`'s own sources.
    let consumer = make_task(vec!["src/**"], vec!["#content:articles"]);
    let project = make_project(vec![("build", consumer)]);
    let changed = vec![PathBuf::from("content/env.cue")];
    let changed_configuration = std::collections::HashSet::from([PathBuf::from("content")]);

    let affected = super::compute_affected_tasks(
        ChangedInputs {
            files: &changed,
            project_root: Path::new("app"),
        },
        &["build".to_string()],
        &AffectedProjects {
            project: &project,
            all_projects: &content_project(),
            checkout_root: None,
            changed_configuration: Some(&changed_configuration),
        },
    );

    assert_eq!(affected, ["build"]);
}

#[test]
fn a_dependency_on_a_step_of_a_sequence_resolves_to_that_step() {
    // `{task: "release[0]"}` as an input makes `deploy` depend on the first
    // step of `release`, by the name `cuenv task` gives it.
    let mut project = make_project(vec![(
        "deploy",
        make_task(vec!["deploy/**"], vec!["release[0]"]),
    )]);
    project.tasks.insert(
        "release".to_string(),
        TaskNode::Sequence(vec![TaskNode::Task(Box::new(make_task(
            vec!["release/**"],
            vec![],
        )))]),
    );
    let all_projects = HashMap::new();

    let affected = affected_in(
        &Change {
            project: &project,
            all_projects: &all_projects,
            files: &[PathBuf::from("docs/readme.md")],
        },
        &["deploy"],
    );

    assert!(affected.is_empty(), "{affected:?}");
}

// ==========================================================================
// A project spelled as a path
// ==========================================================================

/// `content` and, beside it, `sources`, which `content`'s task depends on by
/// a path of its own.
fn content_and_sources() -> HashMap<String, (PathBuf, Project)> {
    let articles = make_task(vec!["articles/**"], vec!["#../sources:pull"]);
    let pull = make_task(vec!["feeds/**"], vec![]);
    HashMap::from([
        (
            "content".to_string(),
            (
                PathBuf::from("content"),
                make_project(vec![("articles", articles)]),
            ),
        ),
        (
            "sources".to_string(),
            (PathBuf::from("sources"), make_project(vec![("pull", pull)])),
        ),
    ])
}

#[test]
fn a_project_spelled_as_a_path_is_found_like_one_spelled_by_name() {
    let all_projects = content_and_sources();
    // By name, from the project that depends on it, and from the root of
    // the checkout: each is the dependency on `content`.
    for spelling in [
        "#content:articles",
        "#../content:articles",
        "#/content:articles",
    ] {
        let consumer = make_task(vec!["src/**"], vec![spelling]);
        let project = make_project(vec![("build", consumer)]);
        let affected_by = |file: &str| {
            affected_in(
                &Change {
                    project: &project,
                    all_projects: &all_projects,
                    files: &[PathBuf::from(file)],
                },
                &["build"],
            )
        };

        assert_eq!(
            affected_by("content/articles/one.md"),
            ["build"],
            "{spelling}"
        );
        // A path written in `content` is relative to `content`.
        assert_eq!(affected_by("sources/feeds/a.xml"), ["build"], "{spelling}");
        // The project was found, so a change to nothing it reads leaves
        // the task out: it is not selected for want of a comparison.
        assert!(affected_by("docs/readme.md").is_empty(), "{spelling}");
    }
}

#[test]
fn a_path_that_names_no_project_counts_as_affected() {
    let all_projects = content_and_sources();
    for spelling in [
        "#../nowhere:articles",
        "#../../outside:articles",
        "#/content/drafts:articles",
    ] {
        let consumer = make_task(vec!["src/**"], vec![spelling]);
        let project = make_project(vec![("build", consumer)]);

        let affected = affected_in(
            &Change {
                project: &project,
                all_projects: &all_projects,
                files: &[PathBuf::from("docs/readme.md")],
            },
            &["build"],
        );

        assert_eq!(affected, ["build"], "{spelling}");
    }
}
