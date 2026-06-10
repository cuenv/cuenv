use super::{TestResult, create_test_dir, init_cue_module, run_cuenv};
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use tempfile::TempDir;

fn write_env(temp_dir: &TempDir, cue_content: &str) -> TestResult {
    fs::write(temp_dir.path().join("env.cue"), cue_content)?;
    Ok(())
}

fn path_arg(path: &Path) -> TestResult<&str> {
    path.to_str()
        .ok_or_else(|| io::Error::other(format!("path is not valid UTF-8: {}", path.display())))
        .map_err(Into::into)
}

fn repo_root() -> TestResult<PathBuf> {
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}

fn copy_cue_files_recursive(source: &Path, destination: &Path) -> TestResult {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_cue_files_recursive(&source_path, &destination_path)?;
        } else if source_path
            .extension()
            .and_then(|extension| extension.to_str())
            == Some("cue")
        {
            fs::copy(&source_path, &destination_path)?;
        }
    }

    Ok(())
}

fn init_local_cuenv_schema_module(dir: &Path) -> TestResult {
    let repo_root = repo_root()?;
    let cue_mod_dir = dir.join("cue.mod");
    fs::create_dir_all(&cue_mod_dir)?;
    fs::copy(
        repo_root.join("cue.mod/module.cue"),
        cue_mod_dir.join("module.cue"),
    )?;
    copy_cue_files_recursive(&repo_root.join("schema"), &dir.join("schema"))?;
    Ok(())
}

fn output_position(output: &str, needle: &'static str) -> TestResult<usize> {
    output
        .find(needle)
        .ok_or_else(|| io::Error::other(format!("missing `{needle}` in output")))
        .map_err(Into::into)
}

#[test]
fn test_schema_typed_embedded_script_runs_from_nested_project_directory() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_local_cuenv_schema_module(temp_dir.path())?;

    let project_dir = temp_dir.path().join("nix");
    fs::create_dir_all(&project_dir)?;
    fs::write(project_dir.join("flake.lock"), "{}")?;
    fs::write(
        project_dir.join("check.sh"),
        "if [ -f flake.lock ]; then echo nested-ok; else echo missing-lock; exit 1; fi\n",
    )?;
    fs::write(
        project_dir.join("env.cue"),
        r#"@extern(embed)

package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "nix"

    tasks: {
        check: schema.#Task & {
            description: "Check nested working directory"
            script:      _ @embed(file=check.sh,type=text)
            scriptShell: "sh"
            hermetic:    false
        }
    }
}
"#,
    )?;

    let (stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(&project_dir)?,
        "--package",
        "cuenv",
        "check",
    ])?;

    assert!(
        success,
        "schema-typed embedded script should run from nested project directory\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("nested-ok"),
        "task should find flake.lock in nested project directory\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    Ok(())
}

#[test]
fn test_task_list_with_shorthand() -> TestResult {
    // Create a temporary directory with test CUE files
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;
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

    write_env(&temp_dir, cue_content)?;

    // Test listing tasks with 't' shorthand
    let (stdout, _stderr, success) =
        run_cuenv(&["t", "-p", path_arg(temp_dir.path())?, "--package", "test"])?;

    assert!(success, "Command should succeed");
    assert!(stdout.contains("Tasks:"), "Should show tasks header");
    assert!(stdout.contains("test_task"), "Should list test_task");
    assert!(stdout.contains("another_task"), "Should list another_task");
    Ok(())
}

#[test]
fn test_task_execution() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;
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

    write_env(&temp_dir, cue_content)?;

    // Test running a task with 'task' command
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "greet",
    ])?;

    assert!(success, "Command should succeed");
    assert!(
        stdout.contains("Hello from task"),
        "Should execute the task"
    );
    assert!(
        stdout.contains("Task 'greet' succeeded"),
        "Should show completion message"
    );
    Ok(())
}

#[test]
fn test_imported_task_working_directory_modes() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;

    let shared_dir = temp_dir.path().join("shared");
    let definition_fixture_dir = shared_dir.join("fixtures");
    let caller_fixture_dir = temp_dir.path().join("fixtures");
    fs::create_dir_all(&definition_fixture_dir)?;
    fs::create_dir_all(&caller_fixture_dir)?;
    fs::write(shared_dir.join("marker.txt"), "definition-root")?;
    fs::write(temp_dir.path().join("marker.txt"), "caller-root")?;
    fs::write(
        definition_fixture_dir.join("marker.txt"),
        "definition-fixture",
    )?;
    fs::write(caller_fixture_dir.join("marker.txt"), "caller-fixture")?;

    fs::write(
        shared_dir.join("tasks.cue"),
        r#"package shared

tasks: {
    readMarker: {
        command: "cat"
        args: ["marker.txt"]
        hermetic: false
    }
    readCallerDefault: {
        command: "cat"
        args: ["marker.txt"]
        hermetic: false
        dir: from: "caller"
    }
}
"#,
    )?;

    write_env(
        &temp_dir,
        r#"package test

import shared "test.example/test/shared"

name: "test"

tasks: {
    definition: shared.tasks.readMarker
    callerDefault: shared.tasks.readCallerDefault & {
        inputs: ["marker.txt"]
    }
    caller: shared.tasks.readMarker & {
        dir: from: "caller"
    }
    definitionSubdir: shared.tasks.readMarker & {
        dir: {
            from: "definition"
            path: "fixtures"
        }
    }
    callerSubdir: shared.tasks.readMarker & {
        dir: {
            from: "caller"
            path: "fixtures"
        }
    }
    moduleRelative: shared.tasks.readMarker & {
        dir: {
            from: "module"
            path: "."
        }
    }
}
"#,
    )?;

    let project_path = path_arg(temp_dir.path())?;

    for (task, marker) in [
        ("definition", "definition-root"),
        ("callerDefault", "caller-root"),
        ("caller", "caller-root"),
        ("definitionSubdir", "definition-fixture"),
        ("callerSubdir", "caller-fixture"),
        ("moduleRelative", "caller-root"),
    ] {
        let (stdout, stderr, success) =
            run_cuenv(&["task", "-p", project_path, "--package", "test", task])?;

        assert!(
            success,
            "task {task} should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            stdout.contains(marker),
            "task {task} should read {marker}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }

    Ok(())
}

#[test]
fn test_task_with_environment_propagation() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;
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

    write_env(&temp_dir, cue_content)?;

    // Test that environment variables are propagated to tasks
    let (stdout, _, success) = run_cuenv(&[
        "t", // Using shorthand
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "check_env",
    ])?;

    assert!(success, "Command should succeed");
    assert!(
        stdout.contains("propagated_value"),
        "Environment variable should be propagated"
    );
    Ok(())
}

#[test]
fn test_exec_command_with_shorthand() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;
    let cue_content = r#"package test

name: "test"

env: {
    EXEC_TEST: "exec_value"
}"#;

    write_env(&temp_dir, cue_content)?;

    // Test exec with 'x' shorthand (changed from 'e' to avoid conflict with -e global flag)
    let (stdout, _, success) = run_cuenv(&[
        "x",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "printenv",
        "EXEC_TEST",
    ])?;

    assert!(success, "Command should succeed");
    assert!(
        stdout.contains("exec_value"),
        "Environment variable should be available to exec command"
    );
    Ok(())
}

#[test]
fn test_exec_with_arguments() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;
    let cue_content = r#"package test

name: "test"

env: {
    PREFIX: "Test"
}"#;

    write_env(&temp_dir, cue_content)?;

    // Test exec with multiple arguments
    let (stdout, _, success) = run_cuenv(&[
        "exec",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "echo",
        "arg1",
        "arg2",
        "arg3",
    ])?;

    assert!(success, "Command should succeed");
    assert!(
        stdout.contains("arg1 arg2 arg3"),
        "All arguments should be passed"
    );
    Ok(())
}

#[test]
fn test_task_sequential_list() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;
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

    write_env(&temp_dir, cue_content)?;

    // Test running a sequential task list
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "sequence",
    ])?;

    assert!(success, "Command should succeed");
    // Check that all tasks ran in sequence
    assert!(stdout.contains("First"), "First task should run");
    assert!(stdout.contains("Second"), "Second task should run");
    assert!(stdout.contains("Third"), "Third task should run");

    // Verify order by checking positions
    let first_pos = output_position(&stdout, "First")?;
    let second_pos = output_position(&stdout, "Second")?;
    let third_pos = output_position(&stdout, "Third")?;
    assert!(first_pos < second_pos, "First should come before Second");
    assert!(second_pos < third_pos, "Second should come before Third");
    Ok(())
}

#[test]
fn test_task_nested_groups() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;
    let cue_content = r#"package test

name: "test"

env: {}

tasks: {
    nested: {
        type: "group"
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

    write_env(&temp_dir, cue_content)?;

    // Test running nested task groups
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "nested",
    ])?;

    assert!(success, "Command should succeed");
    assert!(
        stdout.contains("Subtask 1") || stdout.contains("Subtask 2"),
        "At least one subtask should run"
    );
    Ok(())
}

#[test]
fn test_nested_task_paths_and_aliases() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;
    let cue_content = r#"package test

name: "test"

env: {}

let _t = tasks

tasks: {
    bun: {
        type: "group"
        install: {
            command: "echo"
            args: ["bun install"]
        }
        test: {
            command: "echo"
            args: ["bun test"]
            dependsOn: [_t.bun.install]
        }
    }
}
"#;

    write_env(&temp_dir, cue_content)?;

    // Listing should include canonical dotted paths
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
    ])?;
    assert!(success, "Listing nested tasks should succeed");
    assert!(stdout.contains("bun"), "Should list parent group");
    assert!(
        stdout.contains("install"),
        "Should list nested task install"
    );

    // Execute using dotted path
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "bun.install",
    ])?;
    assert!(success, "Should run nested task via dotted syntax");
    assert!(stdout.contains("bun install"));

    // Execute using colon alias
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "bun:install",
    ])?;
    assert!(success, "Should run nested task via colon syntax");
    assert!(stdout.contains("bun install"));

    // Dependency should resolve to canonical nested name
    let (stdout, _, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "bun.test",
    ])?;
    assert!(success, "Dependent nested task should run");
    assert!(
        stdout.contains("bun install"),
        "Dependency should execute using canonical path"
    );
    assert!(stdout.contains("bun test"));
    Ok(())
}

#[test]
fn test_nonexistent_task_error() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;
    let cue_content = r#"package test

name: "test"

env: {}

tasks: {
    existing: {
        command: "echo"
        args: ["test"]
    }
}"#;

    write_env(&temp_dir, cue_content)?;

    // Test running a nonexistent task
    let (_, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "nonexistent",
    ])?;

    assert!(!success, "Command should fail");
    assert!(
        stderr.contains("not found") || stderr.contains("Task execution failed"),
        "Should report task not found"
    );
    Ok(())
}

#[test]
fn test_exec_command_exit_code() -> TestResult {
    let temp_dir = create_test_dir()?;
    init_cue_module(temp_dir.path())?;
    let cue_content = r"package test

env: {}";

    write_env(&temp_dir, cue_content)?;

    // Test that exec propagates exit codes correctly
    let (_, _, success) = run_cuenv(&[
        "exec",
        "-p",
        path_arg(temp_dir.path())?,
        "--package",
        "test",
        "false", // Command that always fails
    ])?;

    assert!(!success, "Command should fail when executed command fails");
    Ok(())
}
