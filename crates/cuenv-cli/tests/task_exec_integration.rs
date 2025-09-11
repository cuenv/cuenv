//! Integration tests for task and exec commands

use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Helper to run cuenv command and capture output
fn run_cuenv(args: &[&str]) -> (String, String, bool) {
    let output = Command::new("cargo")
        .args(&["run", "--bin", "cuenv", "--"])
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
    
    fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();
    
    // Test listing tasks with 't' shorthand
    let (stdout, _, success) = run_cuenv(&[
        "t",
        "-p", temp_dir.path().to_str().unwrap(),
        "--package", "test"
    ]);
    
    assert!(success, "Command should succeed");
    assert!(stdout.contains("Available tasks:"), "Should show available tasks header");
    assert!(stdout.contains("test_task"), "Should list test_task");
    assert!(stdout.contains("another_task"), "Should list another_task");
}

#[test]
fn test_task_execution() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

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
        "-p", temp_dir.path().to_str().unwrap(),
        "--package", "test",
        "greet"
    ]);
    
    assert!(success, "Command should succeed");
    assert!(stdout.contains("Hello from task"), "Should execute the task");
    assert!(stdout.contains("Task 'greet' completed"), "Should show completion message");
}

#[test]
fn test_task_with_environment_propagation() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

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
        "t",  // Using shorthand
        "-p", temp_dir.path().to_str().unwrap(),
        "--package", "test",
        "check_env"
    ]);
    
    assert!(success, "Command should succeed");
    assert!(stdout.contains("propagated_value"), "Environment variable should be propagated");
}

#[test]
fn test_exec_command_with_shorthand() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

env: {
    EXEC_TEST: "exec_value"
}"#;
    
    fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();
    
    // Test exec with 'e' shorthand
    let (stdout, _, success) = run_cuenv(&[
        "e",
        "-p", temp_dir.path().to_str().unwrap(),
        "--package", "test",
        "printenv",
        "EXEC_TEST"
    ]);
    
    assert!(success, "Command should succeed");
    assert!(stdout.contains("exec_value"), "Environment variable should be available to exec command");
}

#[test]
fn test_exec_with_arguments() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

env: {
    PREFIX: "Test"
}"#;
    
    fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();
    
    // Test exec with multiple arguments
    let (stdout, _, success) = run_cuenv(&[
        "exec",
        "-p", temp_dir.path().to_str().unwrap(),
        "--package", "test",
        "echo",
        "arg1",
        "arg2",
        "arg3"
    ]);
    
    assert!(success, "Command should succeed");
    assert!(stdout.contains("arg1 arg2 arg3"), "All arguments should be passed");
}

#[test]
fn test_task_sequential_list() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

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
        "-p", temp_dir.path().to_str().unwrap(),
        "--package", "test",
        "sequence"
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
        "-p", temp_dir.path().to_str().unwrap(),
        "--package", "test",
        "nested"
    ]);
    
    assert!(success, "Command should succeed");
    assert!(stdout.contains("Subtask 1") || stdout.contains("Subtask 2"), 
            "At least one subtask should run");
}

#[test]
fn test_nonexistent_task_error() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

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
        "-p", temp_dir.path().to_str().unwrap(),
        "--package", "test",
        "nonexistent"
    ]);
    
    assert!(!success, "Command should fail");
    assert!(stderr.contains("not found") || stderr.contains("Task execution failed"), 
            "Should report task not found");
}

#[test]
fn test_exec_command_exit_code() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test

env: {}"#;
    
    fs::write(temp_dir.path().join("test.cue"), cue_content).unwrap();
    
    // Test that exec propagates exit codes correctly
    let (_, _, success) = run_cuenv(&[
        "exec",
        "-p", temp_dir.path().to_str().unwrap(),
        "--package", "test",
        "false"  // Command that always fails
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
            eprintln!("Skipping test - example path doesn't exist: {:?}", example_path);
            return;
        }
        
        // Test listing tasks
        let (stdout, _, success) = run_cuenv(&[
            "t",
            "-p", example_path.to_str().unwrap(),
            "--package", "examples"
        ]);
        
        assert!(success, "Should list tasks successfully");
        assert!(stdout.contains("interpolate"), "Should list interpolate task");
        assert!(stdout.contains("propagate"), "Should list propagate task");
        assert!(stdout.contains("greetAll"), "Should list greetAll task");
        
        // Test running interpolate task
        let (stdout, _, success) = run_cuenv(&[
            "t",
            "-p", example_path.to_str().unwrap(),
            "--package", "examples",
            "interpolate"
        ]);
        
        assert!(success, "Should run interpolate task");
        assert!(stdout.contains("Jack O'Neill"), "Should interpolate environment variable");
        
        // Test running propagate task
        let (stdout, _, success) = run_cuenv(&[
            "t",
            "-p", example_path.to_str().unwrap(),
            "--package", "examples",
            "propagate"
        ]);
        
        assert!(success, "Should run propagate task");
        assert!(stdout.contains("Jack O'Neill"), "Should propagate environment variable");
        
        // Test exec with example environment
        let (stdout, _, success) = run_cuenv(&[
            "e",
            "-p", example_path.to_str().unwrap(),
            "--package", "examples",
            "printenv",
            "NAME"
        ]);
        
        assert!(success, "Should run exec command");
        assert!(stdout.contains("Jack O'Neill"), "Should have environment variable available");
    }
}