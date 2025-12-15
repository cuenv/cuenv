//! Integration tests for task and exec commands

#![allow(clippy::print_stderr)]

use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Helper to run cuenv command and capture output
fn run_cuenv(args: &[&str]) -> (String, String, bool) {
    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");
    let output = Command::new(cuenv_bin)
        .args(args)
        .output()
        .expect("Failed to run cuenv");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let success = output.status.success();

    (stdout, stderr, success)
}

#[test]
fn test_task_list_with_shorthand() {
    // Create a temporary directory with test CUE files
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

name: "test"

env: {
    TEST_VAR: "test_value"
}

tasks: {
    test_task: {
        command: "echo"
        args: ["test"]
    }
    another_task: {
        command: "echo"
        args: ["another"]
    }
}"#;

    // Create cue.mod to ensure module root detection works for source path normalization
    fs::create_dir(temp_dir.path().join("cue.mod")).unwrap();
    fs::write(
        temp_dir.path().join("cue.mod/module.cue"),
        "module: \"test.com\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}",
    )
    .unwrap();
    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

    // Test listing tasks with 't' shorthand
    let (stdout, _stderr, success) = run_cuenv(&[
        "t",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
    ]);

    assert!(success, "Command should succeed");
    assert!(stdout.contains("Tasks:"), "Should show tasks header");
    assert!(stdout.contains("test_task"), "Should list test_task");
    assert!(stdout.contains("another_task"), "Should list another_task");
}

#[test]
fn test_task_execution() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

name: "test"

env: {
    GREETING: "Hello"
    NAME: "World"
}

tasks: {
    greet: {
        command: "echo"
        args: ["Hello from task"]
    }
}"#;

    fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();

    // Test running a task with 'task' command
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "greet",
    ]);

    assert!(success, "Command should succeed");
    assert!(
        stdout.contains("Hello from task"),
        "Should execute the task"
    );
    assert!(
        stdout.contains("Task 'greet' completed"),
        "Should show completion message"
    );
}

#[test]
fn test_task_with_environment_propagation() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

name: "test"

env: {
    TEST_ENV_VAR: "propagated_value"
}

tasks: {
    check_env: {
        command: "printenv"
        args: ["TEST_ENV_VAR"]
    }
}"#;

    fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();

    // Test that environment variables are propagated to tasks
    let (stdout, _, success) = run_cuenv(&[
        "t", // Using shorthand
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "check_env",
    ]);

    assert!(success, "Command should succeed");
    assert!(
        stdout.contains("propagated_value"),
        "Environment variable should be propagated"
    );
}

#[test]
fn test_exec_command_with_shorthand() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

name: "test"

env: {
    EXEC_TEST: "exec_value"
}"#;

    fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();

    // Test exec with 'x' shorthand (changed from 'e' to avoid conflict with -e global flag)
    let (stdout, _, success) = run_cuenv(&[
        "x",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "printenv",
        "EXEC_TEST",
    ]);

    assert!(success, "Command should succeed");
    assert!(
        stdout.contains("exec_value"),
        "Environment variable should be available to exec command"
    );
}

#[test]
fn test_exec_with_arguments() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

name: "test"

env: {
    PREFIX: "Test"
}"#;

    fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();

    // Test exec with multiple arguments
    let (stdout, _, success) = run_cuenv(&[
        "exec",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "echo",
        "arg1",
        "arg2",
        "arg3",
    ]);

    assert!(success, "Command should succeed");
    assert!(
        stdout.contains("arg1 arg2 arg3"),
        "All arguments should be passed"
    );
}

#[test]
fn test_task_sequential_list() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

name: "test"

env: {
    COUNTER: "0"
}

tasks: {
    sequence: [
        {
            command: "echo"
            args: ["First"]
        },
        {
            command: "echo"
            args: ["Second"]
        },
        {
            command: "echo"
            args: ["Third"]
        }
    ]
}"#;

    fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();

    // Test running a sequential task list
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "sequence",
    ]);

    assert!(success, "Command should succeed");
    // Check that all tasks ran in sequence
    assert!(stdout.contains("First"), "First task should run");
    assert!(stdout.contains("Second"), "Second task should run");
    assert!(stdout.contains("Third"), "Third task should run");

    // Verify order by checking positions
    let first_pos = stdout.find("First").unwrap();
    let second_pos = stdout.find("Second").unwrap();
    let third_pos = stdout.find("Third").unwrap();
    assert!(first_pos < second_pos, "First should come before Second");
    assert!(second_pos < third_pos, "Second should come before Third");
}

#[test]
fn test_task_nested_groups() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

name: "test"

env: {}

tasks: {
    nested: {
        subtask1: {
            command: "echo"
            args: ["Subtask 1"]
        }
        subtask2: {
            command: "echo"
            args: ["Subtask 2"]
        }
    }
}"#;

    fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();

    // Test running nested task groups
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "nested",
    ]);

    assert!(success, "Command should succeed");
    assert!(
        stdout.contains("Subtask 1") || stdout.contains("Subtask 2"),
        "At least one subtask should run"
    );
}

#[test]
fn test_nested_task_paths_and_aliases() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

name: "test"

env: {}

tasks: {
    bun: {
        install: {
            command: "echo"
            args: ["bun install"]
        }
        test: {
            command: "echo"
            args: ["bun test"]
            dependsOn: ["install"]
        }
    }
}
"#;

    fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();

    // Listing should include canonical dotted paths
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
    ]);
    assert!(success, "Listing nested tasks should succeed");
    assert!(stdout.contains("bun"), "Should list parent group");
    assert!(
        stdout.contains("install"),
        "Should list nested task install"
    );
    // Tree view doesn't show full dotted path "bun.install"
    // assert!(
    //     stdout.contains("bun.install"),
    //     "Should list nested task with dotted name"
    // );

    // Execute using dotted path
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "bun.install",
    ]);
    assert!(success, "Should run nested task via dotted syntax");
    assert!(stdout.contains("bun install"));

    // Execute using colon alias
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "bun:install",
    ]);
    assert!(success, "Should run nested task via colon syntax");
    assert!(stdout.contains("bun install"));

    // Dependency should resolve to canonical nested name
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "bun.test",
    ]);
    assert!(success, "Dependent nested task should run");
    assert!(
        stdout.contains("bun install"),
        "Dependency should execute using canonical path"
    );
    assert!(stdout.contains("bun test"));
}

#[test]
fn test_nonexistent_task_error() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

name: "test"

env: {}

tasks: {
    existing: {
        command: "echo"
        args: ["test"]
    }
}"#;

    fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();

    // Test running a nonexistent task
    let (_, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "nonexistent",
    ]);

    assert!(!success, "Command should fail");
    assert!(
        stderr.contains("not found") || stderr.contains("Task execution failed"),
        "Should report task not found"
    );
}

#[test]
fn test_exec_command_exit_code() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r"package test

env: {}";

    fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();

    // Test that exec propagates exit codes correctly
    let (_, _, success) = run_cuenv(&[
        "exec",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "false", // Command that always fails
    ]);

    assert!(!success, "Command should fail when executed command fails");
}

#[cfg(test)]
mod test_examples {
    use super::*;

    #[test]
    fn test_task_basic_example() {
        // Get the project root
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let project_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
        let example_path = project_root.join("examples/task-basic");

        // Skip if example doesn't exist
        if !example_path.exists() {
            eprintln!("Skipping test - example path doesn't exist: {example_path:?}");
            return;
        }

        // Test listing tasks
        let (stdout, _, success) = run_cuenv(&[
            "t",
            "-p",
            example_path.to_str().unwrap(),
            "--package",
            "_examples",
        ]);

        assert!(success, "Should list tasks successfully");
        assert!(
            stdout.contains("interpolate"),
            "Should list interpolate task"
        );
        assert!(stdout.contains("propagate"), "Should list propagate task");
        assert!(stdout.contains("greetAll"), "Should list greetAll task");

        // Test running interpolate task
        let (stdout, _, success) = run_cuenv(&[
            "t",
            "-p",
            example_path.to_str().unwrap(),
            "--package",
            "_examples",
            "interpolate",
        ]);

        assert!(success, "Should run interpolate task");
        assert!(
            stdout.contains("Jack O'Neill"),
            "Should interpolate environment variable"
        );

        // Test running propagate task
        let (stdout, _, success) = run_cuenv(&[
            "t",
            "-p",
            example_path.to_str().unwrap(),
            "--package",
            "_examples",
            "propagate",
        ]);

        assert!(success, "Should run propagate task");
        assert!(
            stdout.contains("Jack O'Neill"),
            "Should propagate environment variable"
        );

        // Test exec with example environment (using 'x' shorthand)
        let (stdout, _, success) = run_cuenv(&[
            "x",
            "-p",
            example_path.to_str().unwrap(),
            "--package",
            "_examples",
            "printenv",
            "NAME",
        ]);

        assert!(success, "Should run exec command");
        assert!(
            stdout.contains("Jack O'Neill"),
            "Should have environment variable available"
        );
    }

    #[test]
    fn test_complex_task_dependency_chain() {
        let temp_dir = TempDir::new().unwrap();
        let cue_content = r#"package test

name: "test"

env: {
    COUNTER: "0"
}

tasks: {
    init: {
        command: "echo"
        args: ["Initializing..."]
    }

    build: {
        command: "echo"
        args: ["Building after init"]
        dependsOn: ["init"]
    }

    test: {
        command: "echo"
        args: ["Testing after build"]
        dependsOn: ["build"]
    }

    deploy: {
        command: "echo"
        args: ["Deploying after test"]
        dependsOn: ["test"]
    }
}"#;

        fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();

        // Test running the final task should execute all dependencies
        let (stdout, _, success) = run_cuenv(&[
            "task",
            "-p",
            temp_dir.path().to_str().unwrap(),
            "--package",
            "test",
            "deploy",
        ]);

        assert!(success, "Command should succeed");
        assert!(stdout.contains("Initializing"), "Init task should run");
        assert!(stdout.contains("Building"), "Build task should run");
        assert!(stdout.contains("Testing"), "Test task should run");
        assert!(stdout.contains("Deploying"), "Deploy task should run");

        // Verify execution order
        let init_pos = stdout.find("Initializing").unwrap();
        let build_pos = stdout.find("Building").unwrap();
        let test_pos = stdout.find("Testing").unwrap();
        let deploy_pos = stdout.find("Deploying").unwrap();

        assert!(init_pos < build_pos, "Init should run before build");
        assert!(build_pos < test_pos, "Build should run before test");
        assert!(test_pos < deploy_pos, "Test should run before deploy");
    }

    #[test]
    fn test_task_failure_handling() {
        let temp_dir = TempDir::new().unwrap();
        let cue_content = r#"package test

name: "test"

env: {}

tasks: {
    failing_task: {
        command: "false"  // Command that always fails
        args: []
    }
}"#;

        fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();

        // Test that task failure is properly handled
        let (_, stderr, success) = run_cuenv(&[
            "task",
            "-p",
            temp_dir.path().to_str().unwrap(),
            "--package",
            "test",
            "failing_task",
        ]);

        assert!(!success, "Command should fail");
        assert!(
            stderr.contains("failed") || stderr.contains("error"),
            "Should report failure"
        );
    }

    #[test]
    fn test_mixed_task_types() {
        let temp_dir = TempDir::new().unwrap();
        let cue_content = r#"package test

name: "test"

env: {
    PREFIX: "MIX"
}

tasks: {
    single_task: {
        command: "echo"
        args: [env.PREFIX, "single"]
    }

    sequential_tasks: [
        {
            command: "echo"
            args: [env.PREFIX, "seq1"]
        },
        {
            command: "echo"
            args: [env.PREFIX, "seq2"]
        }
    ]

    parallel_tasks: {
        par1: {
            command: "echo"
            args: [env.PREFIX, "par1"]
        }
        par2: {
            command: "echo"
            args: [env.PREFIX, "par2"]
        }
    }
}"#;

        fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();

        // Test single task
        let (stdout, _, success) = run_cuenv(&[
            "task",
            "-p",
            temp_dir.path().to_str().unwrap(),
            "--package",
            "test",
            "single_task",
        ]);
        assert!(success);
        assert!(stdout.contains("MIX single"));

        // Test sequential tasks
        let (stdout, _, success) = run_cuenv(&[
            "task",
            "-p",
            temp_dir.path().to_str().unwrap(),
            "--package",
            "test",
            "sequential_tasks",
        ]);
        assert!(success);
        assert!(stdout.contains("MIX seq1"));
        assert!(stdout.contains("MIX seq2"));

        // Test parallel tasks
        let (stdout, _, success) = run_cuenv(&[
            "task",
            "-p",
            temp_dir.path().to_str().unwrap(),
            "--package",
            "test",
            "parallel_tasks",
        ]);
        assert!(success);
        // Both parallel tasks should execute
        assert!(stdout.contains("MIX par1") || stdout.contains("MIX par2"));
    }

    #[test]
    fn test_special_characters_in_environment() {
        let temp_dir = TempDir::new().unwrap();
        let cue_content = r#"package test

name: "test"

env: {
    SPECIAL_CHARS: "Hello $USER & $(whoami) | `date` > /dev/null"
    QUOTES: "He said \"Hello world\" and 'goodbye'"
    SPACES: "Value with spaces"
}

tasks: {
    test_special: {
        command: "printenv"
        args: ["SPECIAL_CHARS"]
    }

    test_quotes: {
        command: "printenv"
        args: ["QUOTES"]
    }

    test_spaces: {
        command: "printenv"
        args: ["SPACES"]
    }
}"#;

        fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();

        // Test special characters are passed literally
        let (stdout, _, success) = run_cuenv(&[
            "task",
            "-p",
            temp_dir.path().to_str().unwrap(),
            "--package",
            "test",
            "test_special",
        ]);
        assert!(success);
        assert!(stdout.contains("Hello $USER & $(whoami)"));

        // Test quotes are preserved
        let (stdout, _, success) = run_cuenv(&[
            "task",
            "-p",
            temp_dir.path().to_str().unwrap(),
            "--package",
            "test",
            "test_quotes",
        ]);
        assert!(success);
        assert!(stdout.contains("\"Hello world\""));

        // Test spaces work correctly
        let (stdout, _, success) = run_cuenv(&[
            "task",
            "-p",
            temp_dir.path().to_str().unwrap(),
            "--package",
            "test",
            "test_spaces",
        ]);
        assert!(success);
        assert!(stdout.contains("Value with spaces"));
    }

    #[test]
    fn test_exec_with_complex_args() {
        let temp_dir = TempDir::new().unwrap();
        let cue_content = r#"package test

name: "test"

env: {
    TEST_VAR: "test_value"
}"#;

        fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();

        // Test exec with arguments containing special characters
        let (stdout, _, success) = run_cuenv(&[
            "exec",
            "-p",
            temp_dir.path().to_str().unwrap(),
            "--package",
            "test",
            "echo",
            "arg with spaces",
            "arg\"with\"quotes",
            "arg'with'single'quotes",
            "$TEST_VAR",
            "$(echo 'command substitution')",
        ]);

        assert!(success, "Command should succeed");
        // All arguments should be treated literally
        assert!(stdout.contains("arg with spaces"));
        assert!(stdout.contains("arg\"with\"quotes"));
        assert!(stdout.contains("arg'with'single'quotes"));
        assert!(stdout.contains("$TEST_VAR"));
        assert!(stdout.contains("$(echo 'command substitution')"));
    }
}

#[test]
fn test_task_label_execution_discovers_projects() {
    let temp_dir = TempDir::new().unwrap();
    fs::create_dir(temp_dir.path().join("cue.mod")).unwrap();
    fs::write(
        temp_dir.path().join("cue.mod/module.cue"),
        "module: \"test.com\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}",
    )
    .unwrap();

    let project_a = temp_dir.path().join("project-a");
    fs::create_dir_all(&project_a).unwrap();
    fs::write(
        project_a.join("env.cue"),
        r#"package project_a

name: "project-a"

env: {}

tasks: {
  projen: {
    command: "sh"
    args: ["-c", "echo A-PROJEN"]
    labels: ["projen"]
  }
}
"#,
    )
    .unwrap();

    let project_b = temp_dir.path().join("project-b");
    fs::create_dir_all(&project_b).unwrap();
    fs::write(
        project_b.join("env.cue"),
        r#"package project_b

name: "project-b"

env: {}

tasks: {
  generate: {
    command: "sh"
    args: ["-c", "echo B-PROJEN"]
    labels: ["projen"]
  }
}
"#,
    )
    .unwrap();

    let (stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        project_a.to_str().unwrap(),
        "--package",
        "project_a",
        "-l",
        "projen",
    ]);

    assert!(
        success,
        "Expected success.\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("A-PROJEN"));
    assert!(stdout.contains("B-PROJEN"));
}

#[test]
fn test_task_label_multiple_labels_and_semantics() {
    let temp_dir = TempDir::new().unwrap();
    fs::create_dir(temp_dir.path().join("cue.mod")).unwrap();
    fs::write(
        temp_dir.path().join("cue.mod/module.cue"),
        "module: \"test.com\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}",
    )
    .unwrap();

    // Create a project with multiple tasks having different label combinations
    fs::write(
        temp_dir.path().join("env.cue"),
        r#"package test

name: "test"

env: {}

tasks: {
  unit_tests: {
    command: "sh"
    args: ["-c", "echo UNIT-TESTS"]
    labels: ["test", "unit"]
  }
  e2e_tests: {
    command: "sh"
    args: ["-c", "echo E2E-TESTS"]
    labels: ["test"]
  }
  build: {
    command: "sh"
    args: ["-c", "echo BUILD"]
    labels: ["build"]
  }
}
"#,
    )
    .unwrap();

    // Test: Multiple labels with AND semantics - only unit_tests has both "test" AND "unit"
    let (stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "-l",
        "test",
        "-l",
        "unit",
    ]);

    assert!(
        success,
        "Expected success.\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    // Only unit_tests should match (has both "test" and "unit" labels)
    assert!(
        stdout.contains("UNIT-TESTS"),
        "Should execute unit_tests (has both labels)"
    );
    // e2e_tests only has "test" label, not "unit", so it shouldn't match
    assert!(
        !stdout.contains("E2E-TESTS"),
        "Should NOT execute e2e_tests (missing 'unit' label)"
    );
    assert!(
        !stdout.contains("BUILD"),
        "Should NOT execute build (has neither label)"
    );
}

#[test]
fn test_task_label_error_conflicting_task_name_and_label() {
    let temp_dir = TempDir::new().unwrap();
    fs::create_dir(temp_dir.path().join("cue.mod")).unwrap();
    fs::write(
        temp_dir.path().join("cue.mod/module.cue"),
        "module: \"test.com\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}",
    )
    .unwrap();
    fs::write(
        temp_dir.path().join("env.cue"),
        r#"package test
name: "test"
env: {}
tasks: {
  mytask: {
    command: "echo"
    args: ["hello"]
    labels: ["test"]
  }
}
"#,
    )
    .unwrap();

    // Test: Cannot specify both task name and --label
    let (_stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "mytask",
        "-l",
        "test",
    ]);

    assert!(
        !success,
        "Expected failure when specifying both task name and label"
    );
    assert!(
        stderr.contains("Cannot specify both a task name and --label"),
        "Error message should mention conflict. Got: {stderr}"
    );
}

#[test]
fn test_task_label_error_trailing_args_become_task_name() {
    let temp_dir = TempDir::new().unwrap();
    fs::create_dir(temp_dir.path().join("cue.mod")).unwrap();
    fs::write(
        temp_dir.path().join("cue.mod/module.cue"),
        "module: \"test.com\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}",
    )
    .unwrap();
    fs::write(
        temp_dir.path().join("env.cue"),
        r#"package test
name: "test"
env: {}
tasks: {
  mytask: {
    command: "echo"
    labels: ["test"]
  }
}
"#,
    )
    .unwrap();

    // Test: Trailing args after -- are interpreted as task name (first positional)
    // Since task name conflicts with --label, we get the conflict error
    let (_stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "-l",
        "test",
        "--",
        "arg1",
        "arg2",
    ]);

    assert!(
        !success,
        "Expected failure when using trailing args with label selection"
    );
    // With trailing_var_arg, "arg1" becomes the task name, triggering the conflict error
    assert!(
        stderr.contains("Cannot specify both a task name and --label"),
        "Error message should mention conflict (trailing arg becomes task name). Got: {stderr}"
    );
}

#[test]
fn test_task_label_error_no_matching_tasks() {
    let temp_dir = TempDir::new().unwrap();
    fs::create_dir(temp_dir.path().join("cue.mod")).unwrap();
    fs::write(
        temp_dir.path().join("cue.mod/module.cue"),
        "module: \"test.com\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}",
    )
    .unwrap();
    fs::write(
        temp_dir.path().join("env.cue"),
        r#"package test
name: "test"
env: {}
tasks: {
  mytask: {
    command: "echo"
    args: ["hello"]
    labels: ["existing"]
  }
}
"#,
    )
    .unwrap();

    // Test: No tasks match the given label
    let (_stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "-l",
        "nonexistent",
    ]);

    assert!(!success, "Expected failure when no tasks match label");
    assert!(
        stderr.contains("No tasks with labels") && stderr.contains("nonexistent"),
        "Error message should mention no matching tasks. Got: {stderr}"
    );
}

#[test]
fn test_task_label_error_empty_labels() {
    let temp_dir = TempDir::new().unwrap();
    fs::create_dir(temp_dir.path().join("cue.mod")).unwrap();
    fs::write(
        temp_dir.path().join("cue.mod/module.cue"),
        "module: \"test.com\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}",
    )
    .unwrap();
    fs::write(
        temp_dir.path().join("env.cue"),
        r#"package test
name: "test"
env: {}
tasks: {
  mytask: {
    command: "echo"
    labels: ["test"]
  }
}
"#,
    )
    .unwrap();

    // Test: Empty/whitespace-only labels should error
    let (_stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "-l",
        "   ",
    ]);

    assert!(!success, "Expected failure with empty/whitespace labels");
    assert!(
        stderr.contains("empty") || stderr.contains("whitespace"),
        "Error message should mention empty/whitespace labels. Got: {stderr}"
    );
}
