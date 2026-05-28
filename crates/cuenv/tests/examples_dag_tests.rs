//! Integration tests for DAG building from example environments
//!
//! This module tests that all examples in the `examples/` directory can be
//! loaded and produce valid task DAGs.

use cuengine::evaluate_cue_package_typed;
use cuenv_core::manifest::Project;
use cuenv_core::tasks::{TaskGraph, Tasks};
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
    /// Whether this example is expected to fail CUE evaluation
    /// (e.g., uses non-concrete values that can't be marshaled to JSON)
    expect_eval_failure: bool,
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
///
/// Note: After the module-wide evaluation refactoring (`evaluate_module`), CUE evaluation
/// requires a module root with `cue.mod/module.cue`. The examples are subdirectories within
/// the main project module. This function may fail in test environments where the module
/// structure isn't fully available.
fn load_example_manifest(example_path: &Path) -> TestResult<Project> {
    evaluate_cue_package_typed::<Project>(example_path, "examples").map_err(|e| {
        Box::new(io::Error::other(format!("Failed to load manifest: {e}"))) as Box<dyn Error>
    })
}

fn load_named_example(examples_dir: &Path, name: &str) -> TestResult<Project> {
    load_example_manifest(&examples_dir.join(name)).map_err(|err| {
        Box::new(io::Error::other(format!(
            "Failed to load example '{name}': {err}"
        ))) as Box<dyn Error>
    })
}

/// Check if the FFI/module evaluation is available for these tests.
/// Returns false if examples can't be evaluated due to module root requirements.
fn ffi_available() -> bool {
    get_examples_dir()
        .map(|examples_dir| load_example_manifest(&examples_dir.join("env-basic")).is_ok())
        .unwrap_or(false)
}

/// Skip test with message if FFI is unavailable
macro_rules! skip_if_ffi_unavailable {
    () => {
        if !ffi_available() {
            eprintln!(
                "Skipping test: FFI/module evaluation unavailable (examples need cue.mod root)"
            );
            return Ok(());
        }
    };
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
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "task-output-ref",
            min_task_count: 4, // tmpdir, work, cleanup, pipeline
            has_hooks: false,
            has_env: false,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "task-basic",
            min_task_count: 5, // interpolate, propagate, greetAll, greetIndividual, shellExample
            has_hooks: false,
            has_env: true,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "imported-task-dirs",
            min_task_count: 5, // definition, caller, definitionSubdir, callerSubdir, moduleRelative
            has_hooks: false,
            has_env: false,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "reusable-task-refs",
            min_task_count: 2, // migrate, deploy
            has_hooks: false,
            has_env: false,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "hook",
            min_task_count: 2, // verify_env, show_env
            has_hooks: true,
            has_env: true,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "hook-delayed",
            min_task_count: 2, // status, verify_all
            has_hooks: true,
            has_env: true,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "ci-pipeline",
            min_task_count: 1, // test
            has_hooks: false,
            has_env: false,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "dagger-task",
            min_task_count: 5, // hello, python-info, stage1, stage2, cached-install, etc.
            has_hooks: false,
            has_env: false,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "test-fail",
            min_task_count: 0, // No tasks, just a failing hook
            has_hooks: true,
            has_env: true,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "owners-basic",
            min_task_count: 0, // No tasks, only owners config
            has_hooks: false,
            has_env: true,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "codegen-hello",
            min_task_count: 0, // No tasks, only codegen config for code generation
            has_hooks: false,
            has_env: false,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "ci-onepassword",
            min_task_count: 1, // deploy
            has_hooks: false,
            has_env: true, // Has production environment with 1Password refs
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "ci-infisical",
            min_task_count: 1, // deploy
            has_hooks: false,
            has_env: true, // Has production environment with Infisical refs
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "ci-gcp-secret",
            min_task_count: 1, // deploy
            has_hooks: false,
            has_env: true, // Has production environment with GCP Secret Manager refs
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "ci-aws-secrets",
            min_task_count: 1, // deploy
            has_hooks: false,
            has_env: true, // Has production environment with AWS Secrets Manager refs
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "ci-cachix",
            min_task_count: 1, // build
            has_hooks: false,
            has_env: false,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "ci-namespace-cache",
            min_task_count: 1, // build
            has_hooks: false,
            has_env: false,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "ci-gh-models",
            min_task_count: 1, // eval.prompts
            has_hooks: false,
            has_env: false,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "ci-cuenv-homebrew",
            min_task_count: 1, // build
            has_hooks: false,
            has_env: false,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "ci-cuenv-nix",
            min_task_count: 1, // build
            has_hooks: false,
            has_env: false,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "ci-bun-workspace",
            min_task_count: 1, // version
            has_hooks: false,
            has_env: true, // Has empty env block
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "ci-codecov",
            min_task_count: 1, // test
            has_hooks: false,
            has_env: false,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "container-image",
            min_task_count: 1, // codegen
            has_hooks: false,
            has_env: false,
            expect_eval_failure: false,
        },
        ExampleExpectations {
            name: "vcs-subdir",
            min_task_count: 1, // inspect
            has_hooks: false,
            has_env: false,
            expect_eval_failure: false,
        },
    ]
}

#[test]
fn test_allexamples_load_successfully() -> TestResult {
    skip_if_ffi_unavailable!();

    let examples_dir = get_examples_dir()?;
    let expectations = get_example_expectations();

    for expectation in &expectations {
        let name = expectation.name;
        let example_path = examples_dir.join(name);
        assert!(
            example_path.exists(),
            "Example directory '{name}' does not exist at {example_path:?}",
        );

        let result = load_example_manifest(&example_path);

        if expectation.expect_eval_failure {
            // This example is expected to fail CUE evaluation
            assert!(
                result.is_err(),
                "Example '{name}' was expected to fail evaluation but succeeded",
            );
        } else if let Err(err) = result {
            return Err(Box::new(io::Error::other(format!(
                "Failed to load example '{name}': {err}"
            ))));
        }
    }

    Ok(())
}

#[test]
fn test_allexamples_build_valid_dag() -> TestResult {
    skip_if_ffi_unavailable!();

    let examples_dir = get_examples_dir()?;
    let expectations = get_example_expectations();

    for expectation in &expectations {
        let name = expectation.name;
        // Skip examples that are expected to fail evaluation
        if expectation.expect_eval_failure {
            continue;
        }

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
        let parallel_groups = graph.get_parallel_groups();
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
    skip_if_ffi_unavailable!();

    let examples_dir = get_examples_dir()?;
    let expectations = get_example_expectations();

    for expectation in &expectations {
        let name = expectation.name;
        // Skip examples that are expected to fail evaluation
        if expectation.expect_eval_failure {
            continue;
        }

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
    skip_if_ffi_unavailable!();

    let examples_dir = get_examples_dir()?;
    let expectations = get_example_expectations();

    for expectation in &expectations {
        let name = expectation.name;
        // Skip examples that are expected to fail evaluation
        if expectation.expect_eval_failure {
            continue;
        }

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

            assert!(
                expected_names.contains(&dir_name),
                "Found unexpected example directory '{dir_name}'. Add it to get_example_expectations() or remove it.",
            );
        }
    }

    Ok(())
}

#[test]
fn test_task_basic_specific_tasks() -> TestResult {
    skip_if_ffi_unavailable!();

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
    skip_if_ffi_unavailable!();

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
fn test_dagger_task_loads_successfully() -> TestResult {
    skip_if_ffi_unavailable!();

    // The dagger-task example uses shorthand task references in the `inputs` field
    // (e.g., `{task: "build.deps"}`) using the #TaskOutput type.
    let examples_dir = get_examples_dir()?;
    let manifest = load_named_example(&examples_dir, "dagger-task")?;

    // Verify the dagger-task example has expected tasks
    assert!(
        manifest.tasks.contains_key("hello"),
        "dagger-task example should have 'hello' task"
    );
    assert!(
        manifest.tasks.contains_key("build.deps"),
        "dagger-task example should have 'build.deps' task"
    );

    Ok(())
}

#[test]
fn test_ci_pipeline_has_pipeline_config() -> TestResult {
    skip_if_ffi_unavailable!();

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
    skip_if_ffi_unavailable!();

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
