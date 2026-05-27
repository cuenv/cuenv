#![allow(clippy::unwrap_used)]

use super::{create_test_dir, init_cue_module, run_cuenv};
use std::fs;

#[test]
fn test_task_list_with_shorthand() {
    // Create a temporary directory with test CUE files
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
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
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
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

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

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
        stdout.contains("Task 'greet' succeeded"),
        "Should show completion message"
    );
}

#[test]
fn test_imported_task_working_directory_modes() {
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());

    let shared_dir = temp_dir.path().join("shared");
    let definition_fixture_dir = shared_dir.join("fixtures");
    let caller_fixture_dir = temp_dir.path().join("fixtures");
    fs::create_dir_all(&definition_fixture_dir).unwrap();
    fs::create_dir_all(&caller_fixture_dir).unwrap();
    fs::write(shared_dir.join("marker.txt"), "definition-root").unwrap();
    fs::write(temp_dir.path().join("marker.txt"), "caller-root").unwrap();
    fs::write(
        definition_fixture_dir.join("marker.txt"),
        "definition-fixture",
    )
    .unwrap();
    fs::write(caller_fixture_dir.join("marker.txt"), "caller-fixture").unwrap();

    fs::write(
        shared_dir.join("tasks.cue"),
        r#"package shared

tasks: {
    readMarker: {
        command: "cat"
        args: ["marker.txt"]
        hermetic: false
    }
}
"#,
    )
    .unwrap();

    fs::write(
        temp_dir.path().join("env.cue"),
        r#"package test

import shared "test.example/test/shared"

name: "test"

tasks: {
    definition: shared.tasks.readMarker
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
        dir: "."
    }
}
"#,
    )
    .unwrap();

    for (task, marker) in [
        ("definition", "definition-root"),
        ("caller", "caller-root"),
        ("definitionSubdir", "definition-fixture"),
        ("callerSubdir", "caller-fixture"),
        ("moduleRelative", "caller-root"),
    ] {
        let (stdout, stderr, success) = run_cuenv(&[
            "task",
            "-p",
            temp_dir.path().to_str().unwrap(),
            "--package",
            "test",
            task,
        ]);

        assert!(
            success,
            "task {task} should succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            stdout.contains(marker),
            "task {task} should read {marker}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}

#[test]
fn test_task_with_environment_propagation() {
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
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

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

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
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
    let cue_content = r#"package test

name: "test"

env: {
    EXEC_TEST: "exec_value"
}"#;

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

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
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
    let cue_content = r#"package test

name: "test"

env: {
    PREFIX: "Test"
}"#;

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

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
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
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

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

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
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
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

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

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
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
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

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

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
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
    let cue_content = r#"package test

name: "test"

env: {}

tasks: {
    existing: {
        command: "echo"
        args: ["test"]
    }
}"#;

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

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
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());
    let cue_content = r"package test

env: {}";

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

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
