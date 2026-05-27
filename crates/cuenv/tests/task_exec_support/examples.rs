#![allow(clippy::print_stderr, clippy::unwrap_used)]

use super::{create_test_dir, init_cue_module, run_cuenv};
use std::fs;
use std::path::Path;

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
        "examples",
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
        "examples",
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
        "examples",
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
        "examples",
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
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
    let cue_content = r#"package test

name: "test"

env: {
    COUNTER: "0"
}

let _t = tasks

tasks: {
    init: {
        command: "echo"
        args: ["Initializing..."]
    }

    build: {
        command: "echo"
        args: ["Building after init"]
        dependsOn: [_t.init]
    }

    test: {
        command: "echo"
        args: ["Testing after build"]
        dependsOn: [_t.build]
    }

    deploy: {
        command: "echo"
        args: ["Deploying after test"]
        dependsOn: [_t.test]
    }
}"#;

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

    // Test running the final task should execute all dependencies
    let (stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "deploy",
    ]);

    assert!(
        success,
        "Command should succeed, stdout: {stdout}, stderr: {stderr}"
    );
    assert!(
        stdout.contains("Initializing"),
        "Init task should run, stdout: {stdout}"
    );
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
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
    let cue_content = r#"package test

name: "test"

env: {}

tasks: {
    failing_task: {
        command: "false"  // Command that always fails
        args: []
    }
}"#;

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

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
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
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
        type: "group"
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

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

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
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
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

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

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
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
    let cue_content = r#"package test

name: "test"

env: {
    TEST_VAR: "test_value"
}"#;

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

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
