//! Integration tests for sync scope rules (project vs workspace).
//!
//! These tests assert how `cuenv sync` behaves from root vs nested paths and
//! how `-A` affects CI workflow generation.

// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

const CUENV_BIN: &str = env!("CARGO_BIN_EXE_cuenv");

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root should resolve")
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let file_name = path.file_name().unwrap();
        let dst_path = dst.join(file_name);

        if path.is_dir() {
            copy_dir_recursive(&path, &dst_path);
        } else if path.extension().and_then(|s| s.to_str()) == Some("cue") {
            fs::copy(&path, &dst_path).unwrap();
        }
    }
}

fn write_local_cuenv_module(root: &Path) {
    let cue_mod_dir = root.join("cue.mod");
    fs::create_dir_all(&cue_mod_dir).unwrap();
    fs::write(
        cue_mod_dir.join("module.cue"),
        "module: \"github.com/cuenv/cuenv\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}\n",
    )
    .unwrap();

    let schema_src = repo_root().join("schema");
    let schema_dst = root.join("schema");
    copy_dir_recursive(&schema_src, &schema_dst);
}

fn init_git_repo(root: &Path) {
    let output = Command::new("git")
        .args(["init"])
        .current_dir(root)
        .output()
        .expect("Failed to init git repo");
    assert!(
        output.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(root)
        .output()
        .expect("Failed to configure git email");
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(root)
        .output()
        .expect("Failed to configure git name");
}

fn create_repo() -> TempDir {
    let temp_dir = tempfile::Builder::new()
        .prefix("cuenv_test_")
        .tempdir()
        .expect("Failed to create temp directory");
    let root = temp_dir.path();
    write_local_cuenv_module(root);
    init_git_repo(root);
    temp_dir
}

fn project_env_cue(name: &str, pipeline: &str, task: &str, owner: &str) -> String {
    format!(
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "{name}"

owners: {{
  rules: {{
    project: {{
      pattern: "*"
      owners: ["{owner}"]
    }}
  }}
}}

ci: pipelines: [
  {{
    name: "{pipeline}"
    tasks: ["{task}"]
  }}
]

tasks: {{
  {task}: {{
    command: "echo"
    args: ["{task}"]
    inputs: ["env.cue"]
  }}
}}
"#
    )
}

fn base_env_cue(owner: &str, include_ignore: bool) -> String {
    let ignore_block = if include_ignore {
        r#"
ignore: {
  git: ["target/"]
}
"#
    } else {
        ""
    };

    format!(
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Base

owners: {{
  rules: {{
    default: {{
      pattern: "*"
      owners: ["{owner}"]
    }}
  }}
}}
{ignore_block}
"#
    )
}

fn run_cuenv(current_dir: &Path, args: &[&str]) -> (String, String, bool) {
    let output = Command::new(CUENV_BIN)
        .args(args)
        .current_dir(current_dir)
        .output()
        .expect("Failed to run cuenv");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout, stderr, output.status.success())
}

#[test]
fn sync_root_project_only_generates_root_ci() {
    let tmp = create_repo();
    let root = tmp.path();

    fs::write(
        root.join("env.cue"),
        project_env_cue("root", "build", "build", "@root"),
    )
    .unwrap();

    let (_stdout, stderr, success) = run_cuenv(root, &["sync"]);
    assert!(success, "sync failed: {stderr}");

    let workflows_dir = root.join(".github/workflows");
    assert!(workflows_dir.join("root-build.yml").exists());
    assert!(!workflows_dir.join("service-test.yml").exists());
}

#[test]
fn sync_nested_project_only_generates_nested_ci_in_repo_root() {
    let tmp = create_repo();
    let root = tmp.path();

    fs::write(root.join("env.cue"), base_env_cue("@root", false)).unwrap();

    let nested = root.join("apps/service");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        nested.join("env.cue"),
        project_env_cue("service", "test", "test", "@service"),
    )
    .unwrap();

    let (_stdout, stderr, success) = run_cuenv(&nested, &["sync"]);
    assert!(success, "sync failed: {stderr}");

    let workflows_dir = root.join(".github/workflows");
    assert!(workflows_dir.join("service-test.yml").exists());
    assert!(!workflows_dir.join("root-build.yml").exists());
    assert!(!nested.join(".github").exists());

    let codeowners_path = root.join(".github/CODEOWNERS");
    assert!(codeowners_path.exists());
    let codeowners = fs::read_to_string(&codeowners_path).unwrap();
    assert!(codeowners.contains("@root"));
    assert!(codeowners.contains("@service"));
}

#[test]
fn sync_all_from_nested_generates_all_ci_in_repo_root() {
    let tmp = create_repo();
    let root = tmp.path();

    fs::write(root.join("env.cue"), base_env_cue("@root", false)).unwrap();

    let nested = root.join("apps/service");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        nested.join("env.cue"),
        project_env_cue("service", "test", "test", "@service"),
    )
    .unwrap();

    let other = root.join("apps/api");
    fs::create_dir_all(&other).unwrap();
    fs::write(
        other.join("env.cue"),
        project_env_cue("api", "build", "build", "@api"),
    )
    .unwrap();

    let (_stdout, stderr, success) = run_cuenv(&nested, &["sync", "-A"]);
    assert!(success, "sync -A failed: {stderr}");

    let workflows_dir = root.join(".github/workflows");
    assert!(workflows_dir.join("service-test.yml").exists());
    assert!(workflows_dir.join("api-build.yml").exists());
    assert!(!nested.join(".github").exists());
}

#[test]
fn sync_outside_project_errors() {
    let tmp = create_repo();
    let root = tmp.path();

    fs::write(root.join("env.cue"), base_env_cue("@root", false)).unwrap();

    let nested = root.join("apps/service");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        nested.join("env.cue"),
        project_env_cue("service", "test", "test", "@service"),
    )
    .unwrap();

    let non_project = root.join("shared");
    fs::create_dir_all(&non_project).unwrap();
    fs::write(non_project.join("env.cue"), base_env_cue("@shared", false)).unwrap();

    let (stdout, stderr, success) = run_cuenv(&non_project, &["sync"]);
    assert!(!success, "sync should fail outside a project");

    let output = format!("{stdout}{stderr}");
    assert!(output.contains("project"));
    assert!(output.contains("cuenv"));
    assert!(output.contains("info"));
    assert!(output.contains("-A"));
}

#[test]
fn sync_root_base_only_runs_ignore_and_codeowners() {
    let tmp = create_repo();
    let root = tmp.path();

    fs::write(root.join("env.cue"), base_env_cue("@root", true)).unwrap();

    let nested = root.join("apps/service");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        nested.join("env.cue"),
        project_env_cue("service", "test", "test", "@service"),
    )
    .unwrap();

    let (_stdout, stderr, success) = run_cuenv(root, &["sync"]);
    assert!(success, "sync failed: {stderr}");

    let gitignore = root.join(".gitignore");
    assert!(gitignore.exists());
    let ignore_contents = fs::read_to_string(&gitignore).unwrap();
    assert!(ignore_contents.contains("target/"));

    let codeowners_path = root.join(".github/CODEOWNERS");
    assert!(codeowners_path.exists());

    let workflows_dir = root.join(".github/workflows");
    assert!(!workflows_dir.exists());
}
