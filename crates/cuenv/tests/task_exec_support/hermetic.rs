#![allow(clippy::unwrap_used)]

use super::{create_test_dir, find_in_path, init_cue_module, run_cuenv};
use std::fs;

#[test]
fn test_exec_hermetic_path_no_host_pollution() {
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());

    // Create a minimal project with a custom PATH in env
    let cue_content = r#"package test

name: "test"

env: {
    PATH: "/cuenv/tools/bin"
}"#;

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

    // Run printenv PATH via exec - use absolute path since PATH is hermetic
    let printenv = find_in_path("printenv");
    let (stdout, _stderr, success) = run_cuenv(&[
        "exec",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        &printenv,
        "PATH",
    ]);

    assert!(success, "Command should succeed");

    // PATH should be exactly what we set, not polluted with host paths
    let path = stdout.trim();
    assert_eq!(
        path, "/cuenv/tools/bin",
        "PATH should be exactly what was set in env.cue, not polluted with host PATH. Got: {path}"
    );

    // Verify it does NOT contain common host paths
    assert!(
        !path.contains("/usr/bin"),
        "PATH should not contain /usr/bin (host pollution)"
    );
    assert!(
        !path.contains("/usr/local"),
        "PATH should not contain /usr/local (host pollution)"
    );
    assert!(
        !path.contains("/opt/homebrew"),
        "PATH should not contain /opt/homebrew (host pollution)"
    );
}

#[test]
fn test_task_hermetic_path_no_host_pollution() {
    let temp_dir = create_test_dir();
    init_cue_module(temp_dir.path());

    // Create a project with a task that prints PATH
    // Use absolute path since PATH is hermetic
    let printenv = find_in_path("printenv");
    let cue_content = format!(
        r#"package test

name: "test"

env: {{
    PATH: "/cuenv/tools/bin"
}}

tasks: {{
    show_path: {{
        command: "{printenv}"
        args: ["PATH"]
    }}
}}"#
    );

    fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

    // Run the task
    let (stdout, _stderr, success) = run_cuenv(&[
        "task",
        "-p",
        temp_dir.path().to_str().unwrap(),
        "--package",
        "test",
        "show_path",
    ]);

    assert!(success, "Command should succeed");

    // Extract PATH from output (task output includes other info)
    let path_line = stdout
        .lines()
        .find(|line| line.starts_with("/cuenv/tools/bin") || line.contains("PATH"))
        .unwrap_or("");

    // PATH should be exactly what we set
    assert!(
        path_line.contains("/cuenv/tools/bin"),
        "PATH should contain our custom path. Got output: {stdout}"
    );
    assert!(
        !path_line.contains("/usr/bin"),
        "PATH should not contain /usr/bin (host pollution). Got: {path_line}"
    );
    assert!(
        !path_line.contains("/usr/local"),
        "PATH should not contain /usr/local (host pollution). Got: {path_line}"
    );
}
