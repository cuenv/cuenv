use super::{create_test_dir, init_cue_module, run_cuenv};
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn project_root() -> TestResult<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let crates_dir = manifest_dir
        .parent()
        .ok_or_else(|| io::Error::other("cuenv crate directory has no parent"))?;
    let repo_root = crates_dir
        .parent()
        .ok_or_else(|| io::Error::other("crates directory has no parent"))?;
    Ok(repo_root.to_path_buf())
}

fn write_env(temp_dir: &TempDir, cue_content: &str) -> TestResult {
    fs::write(temp_dir.path().join("env.cue"), cue_content)?;
    Ok(())
}

fn path_arg(path: &Path) -> TestResult<&str> {
    path.to_str()
        .ok_or_else(|| io::Error::other(format!("path is not valid UTF-8: {}", path.display())))
        .map_err(Into::into)
}

fn output_position(output: &str, needle: &'static str) -> TestResult<usize> {
    output
        .find(needle)
        .ok_or_else(|| io::Error::other(format!("missing `{needle}` in output")))
        .map_err(Into::into)
}

#[test]
fn test_task_basic_example() -> TestResult {
    let example_path = project_root()?.join("examples/task-basic");

    if !example_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("example path does not exist: {}", example_path.display()),
        )
        .into());
    }
    let example_path_arg = path_arg(&example_path)?;

    // Test listing tasks
    let (stdout, _, success) = run_cuenv(&["t", "-p", example_path_arg, "--package", "examples"]);

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
        example_path_arg,
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
        example_path_arg,
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
        example_path_arg,
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
    Ok(())
}

#[test]
fn test_complex_task_dependency_chain() -> TestResult {
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

    write_env(&temp_dir, cue_content)?;
    let project_path = path_arg(temp_dir.path())?;

    // Test running the final task should execute all dependencies
    let (stdout, stderr, success) =
        run_cuenv(&["task", "-p", project_path, "--package", "test", "deploy"]);

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
    let init_pos = output_position(&stdout, "Initializing")?;
    let build_pos = output_position(&stdout, "Building")?;
    let test_pos = output_position(&stdout, "Testing")?;
    let deploy_pos = output_position(&stdout, "Deploying")?;

    assert!(init_pos < build_pos, "Init should run before build");
    assert!(build_pos < test_pos, "Build should run before test");
    assert!(test_pos < deploy_pos, "Test should run before deploy");
    Ok(())
}

#[test]
fn test_task_failure_handling() -> TestResult {
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

    write_env(&temp_dir, cue_content)?;

    // Test that task failure is properly handled
    let (_, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "failing_task",
    ]);

    assert!(!success, "Command should fail");
    assert!(
        stderr.contains("failed") || stderr.contains("error"),
        "Should report failure"
    );
    Ok(())
}

#[test]
fn test_mixed_task_types() -> TestResult {
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

    write_env(&temp_dir, cue_content)?;
    let project_path = path_arg(temp_dir.path())?;

    // Test single task
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        project_path,
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
        project_path,
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
        project_path,
        "--package",
        "test",
        "parallel_tasks",
    ]);
    assert!(success);
    // Both parallel tasks should execute
    assert!(stdout.contains("MIX par1") || stdout.contains("MIX par2"));
    Ok(())
}

#[test]
fn test_special_characters_in_environment() -> TestResult {
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

    write_env(&temp_dir, cue_content)?;
    let project_path = path_arg(temp_dir.path())?;

    // Test special characters are passed literally
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        project_path,
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
        project_path,
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
        project_path,
        "--package",
        "test",
        "test_spaces",
    ]);
    assert!(success);
    assert!(stdout.contains("Value with spaces"));
    Ok(())
}

#[test]
fn test_exec_with_complex_args() -> TestResult {
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
    let cue_content = r#"package test

name: "test"

env: {
    TEST_VAR: "test_value"
}"#;

    write_env(&temp_dir, cue_content)?;

    // Test exec with arguments containing special characters
    let (stdout, _, success) = run_cuenv(&[
        "exec",
        "-p",
        path_arg(temp_dir.path())?,
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
    Ok(())
}
