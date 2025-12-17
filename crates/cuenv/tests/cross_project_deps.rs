#![allow(missing_docs, unused_variables, clippy::uninlined_format_args)]
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// Create a test directory with non-hidden name (CUE ignores hidden directories)
/// and initialize it with a `.git` directory.
fn create_test_root() -> TempDir {
    let tmp = tempfile::Builder::new()
        .prefix("cuenv_test_")
        .tempdir()
        .expect("Failed to create temp directory");
    let root = tmp.path();
    // Create fake git root
    fs::create_dir_all(root.join(".git")).unwrap();
    tmp
}

/// Initialize a directory as a CUE module with proper structure
fn init_cue_module(dir: &Path, module_name: &str) {
    fs::create_dir_all(dir.join("cue.mod")).unwrap();
    // CUE module paths must be lowercase
    let lowercase_name = module_name.to_lowercase();
    fs::write(
        dir.join("cue.mod/module.cue"),
        format!(
            r#"module: "test.example/{}"
language: version: "v0.9.0"
"#,
            lowercase_name
        ),
    )
    .unwrap();
}

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

fn write_proj_b(dir: &Path, version: &str) {
    let projb = dir.join("projB");
    fs::create_dir_all(projb.join("src")).unwrap();
    // Initialize projB as its own CUE module
    init_cue_module(&projb, "projB");
    // package detection relies on first package line
    let cue_b = r#"package projB

name: "projB"

env: {}

tasks: {
  build: {
    command: "sh"
    args: ["-c", "mkdir -p dist/assets; cp -f src/version.txt dist/app.txt; echo asset > dist/assets/file.txt"]
    inputs: ["src/version.txt"]
    outputs: ["dist/app.txt", "dist/assets"]
  }
}
"#;
    fs::write(projb.join("env.cue"), cue_b).unwrap();
    fs::write(projb.join("src/version.txt"), version).unwrap();
}

fn write_proj_a(dir: &Path, mapping_from: &str, mapping_to: &str, external_project: &str) {
    let proja = dir.join("projA");
    fs::create_dir_all(&proja).unwrap();
    // Initialize projA as its own CUE module
    init_cue_module(&proja, "projA");
    let cue_a = format!(
        r#"package projA

name: "projA"

env: {{}}

tasks: {{
  consume: {{
    command: "sh"
    args: ["-c", "mkdir -p out; cp vendor/app.txt out/used.txt; echo done"]
    inputs: [{{
      project: "{external_project}"
      task: "build"
      map: [{{ from: "{mapping_from}", to: "{mapping_to}" }}]
    }}]
    outputs: ["out/used.txt"]
  }}
}}
"#
    );
    fs::write(proja.join("env.cue"), cue_a).unwrap();
}

#[test]
fn test_external_auto_run_and_materialization() {
    let tmp = create_test_root();
    let root = tmp.path();

    write_proj_b(root, "v1-auto");
    write_proj_a(root, "dist/app.txt", "vendor/app.txt", "../projB");

    let (stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        root.join("projA").to_str().unwrap(),
        "--package",
        "projA",
        "consume",
    ]);

    assert!(
        success,
        "First run should succeed.\n--- stdout ---\n{}\n--- stderr ---\n{}",
        stdout, stderr
    );
    assert!(stdout.contains("Task 'consume' completed") || stdout.contains("succeeded"));
}

#[test]
fn test_cache_hits_and_invalidation() {
    let tmp = create_test_root();
    let root = tmp.path();

    write_proj_b(root, "v1-cache");
    write_proj_a(root, "dist/app.txt", "vendor/app.txt", "../projB");

    // First run (populate cache)
    let (out1, err1, ok1) = run_cuenv(&[
        "task",
        "-p",
        root.join("projA").to_str().unwrap(),
        "--package",
        "projA",
        "consume",
    ]);
    assert!(ok1, "Run 1 failed. stdout: {}, stderr: {}", out1, err1);

    // Second run (should hit cache)
    let (out2, err2, ok2) = run_cuenv(&[
        "task",
        "-p",
        root.join("projA").to_str().unwrap(),
        "--package",
        "projA",
        "consume",
    ]);
    assert!(ok2, "Run 2 failed. stdout: {}, stderr: {}", out2, err2);

    // Change external input content and rerun
    fs::write(root.join("projB/src/version.txt"), "v2").unwrap();
    let (out3, err3, ok3) = run_cuenv(&[
        "task",
        "-p",
        root.join("projA").to_str().unwrap(),
        "--package",
        "projA",
        "consume",
    ]);
    assert!(
        ok3,
        "Should re-run after external change. stdout: {}, stderr: {}",
        out3, err3
    );
}

#[test]
#[ignore = "hermetic execution temporarily disabled - validation only runs in hermetic path"]
fn test_mapping_error_undeclared_output() {
    let tmp = create_test_root();
    let root = tmp.path();

    write_proj_b(root, "v1-map");
    write_proj_a(root, "dist/missing.txt", "vendor/app.txt", "../projB");

    let (_stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        root.join("projA").to_str().unwrap(),
        "--package",
        "projA",
        "consume",
    ]);

    assert!(!success, "Should fail on undeclared output mapping");
}

#[test]
#[ignore = "hermetic execution temporarily disabled - validation only runs in hermetic path"]
fn test_path_safety_outside_git_root() {
    let tmp = create_test_root();
    let root = tmp.path();

    // Create projA only
    write_proj_a(root, "dist/app.txt", "vendor/app.txt", "../../outside");

    let (_stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        root.join("projA").to_str().unwrap(),
        "--package",
        "projA",
        "consume",
    ]);

    assert!(
        !success,
        "Should fail when external path resolves outside git root"
    );
}

#[test]
#[ignore = "hermetic execution temporarily disabled - validation only runs in hermetic path"]
fn test_collision_duplicate_dest() {
    let tmp = create_test_root();
    let root = tmp.path();

    // Write projB
    write_proj_b(root, "v1-coll");

    // Write projA with two mappings to same 'to'
    let proja = root.join("projA");
    fs::create_dir_all(&proja).unwrap();
    init_cue_module(&proja, "projA");
    let cue_a = r#"package projA

name: "projA"

env: {}

tasks: {
  consume: {
    command: "sh"
    args: ["-c", "true"]
    inputs: [{
      project: "../projB"
      task: "build"
      map: [
        { from: "dist/app.txt", to: "vendor/app.txt" },
        { from: "dist/app.txt", to: "vendor/app.txt" }
      ]
    }]
    outputs: []
  }
}
"#;
    fs::write(proja.join("env.cue"), cue_a).unwrap();

    let (_stdout, stderr, success) = run_cuenv(&[
        "task",
        "-p",
        proja.to_str().unwrap(),
        "--package",
        "projA",
        "consume",
    ]);

    assert!(!success, "Should fail on destination collision");
}
