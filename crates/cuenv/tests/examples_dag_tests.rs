//! Integration tests for DAG building from example environments
//!
//! This module tests that all examples in the `examples/` directory can be
//! loaded and produce valid task DAGs.

use cuenv::commands::CommandExecutor;
use cuenv_core::manifest::Project;
use cuenv_core::tasks::Tasks;
use cuenv_task_exec::TaskGraph;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::{fs, io};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// Expected properties for each example directory
struct ExampleExpectations {
    /// Directory name under examples/
    name: &'static str,
    /// Minimum number of top-level tasks expected
    min_task_count: usize,
    /// Whether the example defines hooks
    has_hooks: bool,
    /// Whether the example defines environment variables
    has_env: bool,
}

/// Get the path to the examples directory
fn get_examples_dir() -> TestResult<PathBuf> {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Ok(Path::new(manifest_dir)
        .parent() // crates
        .and_then(|p| p.parent()) // project root
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "failed to find project root"))?
        .join("examples"))
}

/// Load a Project manifest from an example directory.
fn load_example_manifest(example_path: &Path) -> TestResult<Project> {
    let directory = example_path.canonicalize()?;
    let executor = CommandExecutor::new("examples".to_string());
    let module = executor.module(&directory)?;
    let relative = directory.strip_prefix(&module.root)?;
    let instance = module.get(relative).ok_or_else(|| {
        io::Error::other(format!("No evaluated project in {}", directory.display()))
    })?;
    Ok(instance.to_project()?)
}

fn load_named_example(examples_dir: &Path, name: &str) -> TestResult<Project> {
    load_example_manifest(&examples_dir.join(name)).map_err(|err| {
        Box::new(io::Error::other(format!(
            "Failed to load example '{name}': {err}"
        ))) as Box<dyn Error>
    })
}

/// Build a `TaskGraph` from a `Project` manifest and validate it
fn build_and_validate_graph(manifest: &Project) -> TestResult<TaskGraph> {
    // Convert the manifest tasks to a Tasks struct
    let tasks = Tasks {
        tasks: manifest.tasks.clone(),
    };

    let mut graph = TaskGraph::new();

    // Build the complete graph from all tasks
    graph
        .build_complete_graph(&tasks)
        .map_err(|e| io::Error::other(format!("Failed to build graph: {e}")))?;

    Ok(graph)
}

/// Define expected properties for all examples
fn get_example_expectations() -> Vec<ExampleExpectations> {
    vec![
        ExampleExpectations {
            name: "env-basic",
            min_task_count: 0, // No tasks, only env vars
            has_hooks: false,
            has_env: true,
        },
        ExampleExpectations {
            name: "env-contexts",
            min_task_count: 2,
            has_hooks: false,
            has_env: true,
        },
        ExampleExpectations {
            name: "task-output-ref",
            min_task_count: 4, // tmpdir, work, cleanup, pipeline
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "task-basic",
            min_task_count: 5, // interpolate, propagate, greetAll, greetIndividual, shellExample
            has_hooks: false,
            has_env: true,
        },
        ExampleExpectations {
            name: "imported-task-dirs",
            min_task_count: 5, // definition, caller, definitionSubdir, callerSubdir, moduleRelative
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "file-references/app",
            min_task_count: 1, // render, reading another project's and the module's files
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "contrib-rust-task",
            min_task_count: 1, // fmt
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "reusable-task-refs",
            min_task_count: 2, // migrate, deploy
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "hook",
            min_task_count: 2, // verify_env, show_env
            has_hooks: true,
            has_env: true,
        },
        ExampleExpectations {
            name: "hook-delayed",
            min_task_count: 2, // status, verify_all
            has_hooks: true,
            has_env: true,
        },
        ExampleExpectations {
            name: "ci-pipeline",
            min_task_count: 1, // test
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "test-fail",
            min_task_count: 0, // No tasks, just a failing hook
            has_hooks: true,
            has_env: true,
        },
        ExampleExpectations {
            name: "codegen-hello",
            min_task_count: 0, // No tasks, only codegen config for code generation
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "ci-onepassword",
            min_task_count: 1, // deploy
            has_hooks: false,
            has_env: true, // Has production environment with 1Password refs
        },
        ExampleExpectations {
            name: "ci-infisical",
            min_task_count: 1, // deploy
            has_hooks: false,
            has_env: true, // Has production environment with Infisical refs
        },
        ExampleExpectations {
            name: "ci-infisical-oidc",
            min_task_count: 1, // deploy
            has_hooks: false,
            has_env: true, // Infisical refs resolved with GitHub OIDC
        },
        ExampleExpectations {
            name: "ci-infisical-action",
            min_task_count: 1, // deploy
            has_hooks: false,
            has_env: false, // Secrets come from Infisical/secrets-action
        },
        ExampleExpectations {
            name: "ci-gcp-secret",
            min_task_count: 1, // deploy
            has_hooks: false,
            has_env: true, // Has production environment with GCP Secret Manager refs
        },
        ExampleExpectations {
            name: "ci-aws-secrets",
            min_task_count: 1, // deploy
            has_hooks: false,
            has_env: true, // Has production environment with AWS Secrets Manager refs
        },
        ExampleExpectations {
            name: "services-readiness",
            min_task_count: 1, // prepare
            has_hooks: false,
            has_env: false, // Services + a prepare task; no env block
        },
        ExampleExpectations {
            name: "infrastructure-random",
            min_task_count: 0, // Managed resources only
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "ci-cachix",
            min_task_count: 1, // build
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "ci-hestia",
            min_task_count: 1, // build
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "ci-namespace-cache",
            min_task_count: 1, // build
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "ci-gh-models",
            min_task_count: 1, // eval.prompts
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "ci-cuenv-homebrew",
            min_task_count: 1, // build
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "ci-cuenv-nix",
            min_task_count: 1, // build
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "ci-bun-workspace",
            min_task_count: 1, // version
            has_hooks: false,
            has_env: true, // Has empty env block
        },
        ExampleExpectations {
            name: "ci-codecov",
            min_task_count: 1, // test
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "container-image",
            min_task_count: 1, // codegen
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "task-captures",
            min_task_count: 2, // build, report
            has_hooks: false,
            has_env: false,
        },
        ExampleExpectations {
            name: "vcs-subdir",
            min_task_count: 1, // inspect
            has_hooks: false,
            has_env: false,
        },
    ]
}

#[test]
fn test_allexamples_load_successfully() -> TestResult {
    let examples_dir = get_examples_dir()?;
    let expectations = get_example_expectations();

    for expectation in &expectations {
        let name = expectation.name;
        let example_path = examples_dir.join(name);
        assert!(
            example_path.exists(),
            "Example directory '{name}' does not exist at {example_path:?}",
        );

        load_example_manifest(&example_path)
            .map_err(|err| io::Error::other(format!("Failed to load example '{name}': {err}")))?;
    }

    Ok(())
}

#[test]
fn test_allexamples_build_valid_dag() -> TestResult {
    let examples_dir = get_examples_dir()?;
    let expectations = get_example_expectations();

    for expectation in &expectations {
        let name = expectation.name;
        let manifest = load_named_example(&examples_dir, name)?;

        // Validate task count
        let task_count = manifest.tasks.len();
        let min_task_count = expectation.min_task_count;
        assert!(
            task_count >= min_task_count,
            "Example '{name}' has {task_count} tasks, expected at least {min_task_count}",
        );

        // Build and validate the graph
        let graph = build_and_validate_graph(&manifest).map_err(|err| {
            Box::new(io::Error::other(format!(
                "Failed to build graph for '{name}': {err}"
            ))) as Box<dyn Error>
        })?;

        // Validate no cycles
        assert!(
            !graph.has_cycles(),
            "Example '{name}' has cyclic dependencies",
        );

        // Validate topological sort succeeds
        let sorted = graph.topological_sort();
        let topo_err = sorted.as_ref().err();
        assert!(
            sorted.is_ok(),
            "Topological sort failed for '{name}': {topo_err:?}",
        );

        // Validate parallel groups can be computed
        let parallel_groups = graph.parallel_groups();
        let parallel_err = parallel_groups.as_ref().err();
        assert!(
            parallel_groups.is_ok(),
            "Failed to compute parallel groups for '{name}': {parallel_err:?}",
        );
    }

    Ok(())
}

#[test]
fn test_allexamples_have_expected_hooks() -> TestResult {
    let examples_dir = get_examples_dir()?;
    let expectations = get_example_expectations();

    for expectation in &expectations {
        let name = expectation.name;
        let manifest = load_named_example(&examples_dir, name)?;

        let has_hooks = manifest
            .hooks
            .as_ref()
            .is_some_and(|hooks| hooks.on_enter.is_some() || hooks.on_exit.is_some());

        let expected_has_hooks = expectation.has_hooks;
        assert_eq!(
            has_hooks, expected_has_hooks,
            "Example '{name}' hooks expectation mismatch: expected has_hooks={expected_has_hooks}, got={has_hooks}",
        );
    }

    Ok(())
}

#[test]
fn test_allexamples_have_expected_env() -> TestResult {
    let examples_dir = get_examples_dir()?;
    let expectations = get_example_expectations();

    for expectation in &expectations {
        let name = expectation.name;
        let manifest = load_named_example(&examples_dir, name)?;

        let has_env = manifest.env.is_some();

        let expected_has_env = expectation.has_env;
        assert_eq!(
            has_env, expected_has_env,
            "Example '{name}' env expectation mismatch: expected has_env={expected_has_env}, got={has_env}",
        );
    }

    Ok(())
}

/// Example directories that are `.rules.cue` files (`DirectoryRules`) rather
/// than `Project` manifests (`env.cue`).
///
/// These cannot be loaded as a `Project` (they have no `name` field), so they
/// are deliberately excluded from `get_example_expectations()` and validated
/// separately by `test_rules_examples_evaluate`, which evaluates them through
/// the real `cuenv sync` `.rules.cue` path.
const RULES_ONLY_EXAMPLES: &[&str] = &[
    "rules-gitignore",
    "rules-dockerignore",
    "rules-codeowners-precedence",
];

#[test]
fn test_no_unexpected_example_directories() -> TestResult {
    let examples_dir = get_examples_dir()?;
    let expectations = get_example_expectations();
    let expected_names: Vec<&str> = expectations.iter().map(|e| e.name).collect();

    // Container directories that hold test fixtures, not standalone examples
    let test_fixture_containers = ["contributor-tests"];

    let entries = fs::read_dir(&examples_dir)?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            let dir_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "invalid example directory name")
                })?;

            // Skip hidden directories
            if dir_name.starts_with('.') {
                continue;
            }

            // Skip test fixture containers (they contain sub-examples, not standalone env.cue)
            if test_fixture_containers.contains(&dir_name) {
                continue;
            }

            // Rules-only examples are tracked and validated separately.
            if RULES_ONLY_EXAMPLES.contains(&dir_name) {
                continue;
            }

            // An example of several projects lists each as `<example>/<project>`.
            assert!(
                expected_names
                    .iter()
                    .any(|name| name.split('/').next() == Some(dir_name)),
                "Found unexpected example directory '{dir_name}'. Add it to get_example_expectations() (Project examples) or RULES_ONLY_EXAMPLES (.rules.cue examples), or remove it.",
            );
        }
    }

    Ok(())
}

/// Every `.rules.cue` example must evaluate through the real `cuenv sync`
/// `.rules.cue` path into a valid `DirectoryRules`. This gives the example
/// files actual coverage (the `schema-coverage-matrix.md` rows cite them as
/// fixtures) and proves each example parses with the field-enforcing serde
/// boundary applied.
#[test]
fn test_rules_examples_evaluate() -> TestResult {
    let examples_dir = get_examples_dir()?;

    for name in RULES_ONLY_EXAMPLES {
        let rules_file = examples_dir.join(name).join(".rules.cue");
        assert!(
            rules_file.exists(),
            "Rules example '{name}' is missing its .rules.cue file at {rules_file:?}",
        );

        let rules = cuenv::providers::evaluate_rules_file(&rules_file).map_err(|err| {
            Box::new(io::Error::other(format!(
                "Failed to evaluate rules example '{name}': {err}"
            ))) as Box<dyn Error>
        })?;

        // Each example must populate at least the rule family it demonstrates.
        let populated =
            rules.ignore.is_some() || rules.owners.is_some() || rules.editorconfig.is_some();
        assert!(
            populated,
            "Rules example '{name}' evaluated to empty DirectoryRules",
        );
    }

    Ok(())
}

#[test]
fn test_task_basic_specific_tasks() -> TestResult {
    let examples_dir = get_examples_dir()?;
    let manifest = load_named_example(&examples_dir, "task-basic")?;

    // Check for specific expected tasks
    let expected_tasks = [
        "interpolate",
        "propagate",
        "greetAll",
        "greetIndividual",
        "shellExample",
    ];

    for task_name in &expected_tasks {
        assert!(
            manifest.tasks.contains_key(*task_name),
            "task-basic example missing expected task '{task_name}'",
        );
    }

    Ok(())
}

#[test]
fn test_reusable_task_refs_example_preserves_dependencies() -> TestResult {
    let examples_dir = get_examples_dir()?;
    let manifest = load_named_example(&examples_dir, "reusable-task-refs")?;

    let deploy = manifest
        .tasks
        .get("deploy")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "deploy task should exist"))?;
    let dep_names: Vec<&str> = deploy
        .depends_on()
        .iter()
        .map(|dep| dep.task_name())
        .collect();
    assert_eq!(dep_names, vec!["migrate"]);

    let pipeline_task = manifest
        .ci
        .as_ref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "ci should exist"))?
        .pipelines
        .get("default")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "default pipeline should exist"))?
        .tasks
        .first()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "default pipeline should have one task",
            )
        })?;

    assert_eq!(pipeline_task.task_name(), "deploy");

    Ok(())
}

#[test]
fn test_ci_pipeline_has_pipeline_config() -> TestResult {
    let examples_dir = get_examples_dir()?;
    let manifest = load_named_example(&examples_dir, "ci-pipeline")?;

    // CI pipeline should have ci configuration
    assert!(
        manifest.ci.is_some(),
        "ci-pipeline example should have ci configuration"
    );

    let ci = manifest
        .ci
        .as_ref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "ci should exist"))?;
    assert!(
        !ci.pipelines.is_empty(),
        "ci-pipeline should have at least one pipeline defined"
    );

    Ok(())
}

#[test]
fn test_hook_delayed_has_ordered_hooks() -> TestResult {
    let examples_dir = get_examples_dir()?;
    let manifest = load_named_example(&examples_dir, "hook-delayed")?;

    let hooks = manifest.on_enter_hooks();

    // Should have at least 3 hooks with different orders
    let hook_count = hooks.len();
    assert!(
        hook_count >= 3,
        "hook-delayed should have at least 3 onEnter hooks, found {hook_count}",
    );

    // Verify hooks are sorted by order
    let orders: Vec<i32> = hooks.iter().map(|h| h.order).collect();
    let mut sorted_orders = orders.clone();
    sorted_orders.sort_unstable();

    assert_eq!(
        orders, sorted_orders,
        "Hooks should be returned in sorted order"
    );

    Ok(())
}
