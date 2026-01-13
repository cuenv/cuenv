#![allow(missing_docs)]
// Integration tests can use unwrap/expect for cleaner assertions
#![allow(clippy::unwrap_used, clippy::expect_used)]

use cuengine::evaluate_cue_package_typed;
use cuenv_core::manifest::{Base, Project};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// Create a Command with a clean environment (no CI vars leaking).
fn clean_environment_command(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("USER", std::env::var("USER").unwrap_or_default());
    cmd
}

/// Create a test directory with non-hidden prefix for CUE loader compatibility.
fn create_test_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("cuenv_test_")
        .tempdir()
        .expect("Failed to create temp directory")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root should resolve")
}

fn write_local_cuenv_module(root: &Path) {
    fs::create_dir_all(root.join("cue.mod")).unwrap();
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"github.com/cuenv/cuenv\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}\n",
    )
    .unwrap();

    // Copy the real schema package into the temporary module so imports work.
    let schema_src = repo_root().join("schema");
    let schema_dst = root.join("schema");
    copy_dir_recursive(&schema_src, &schema_dst);
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

#[test]
fn project_name_is_required_by_schema() {
    let tmp = create_test_dir();
    let root = tmp.path();
    write_local_cuenv_module(root);

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  // name intentionally omitted
}
"#,
    )
    .unwrap();

    let res = evaluate_cue_package_typed::<Project>(root, "cuenv");
    assert!(res.is_err(), "schema should reject missing `name`");
}

#[test]
fn project_name_cannot_be_empty() {
    let tmp = create_test_dir();
    let root = tmp.path();
    write_local_cuenv_module(root);

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: ""
}
"#,
    )
    .unwrap();

    let res = evaluate_cue_package_typed::<Project>(root, "cuenv");
    assert!(res.is_err(), "schema should reject empty `name`");
}

#[test]
fn base_can_be_composed_standalone() {
    let tmp = create_test_dir();
    let root = tmp.path();
    write_local_cuenv_module(root);

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Base & {
  env: {
    HELLO: "world"
  }
}
"#,
    )
    .unwrap();

    let base = evaluate_cue_package_typed::<Base>(root, "cuenv").expect("Base should evaluate");
    assert!(base.env.is_some());
}

#[test]
fn task_command_with_base_schema_shows_helpful_error() {
    let tmp = create_test_dir();
    let root = tmp.path();
    write_local_cuenv_module(root);

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Base & {
  env: {
    HELLO: "world"
  }
}
"#,
    )
    .unwrap();

    // Try to execute task command (which requires schema.#Project)
    let cuenv_bin = env!("CARGO_BIN_EXE_cuenv");
    let output = clean_environment_command(cuenv_bin)
        .args(["task", "--path", root.to_str().unwrap()])
        .output()
        .expect("Failed to run cuenv");

    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "should fail with Base schema");
    // Note: miette wraps long lines with │ characters, so we check for parts separately
    // to avoid failures due to line-break positions.
    assert!(
        stderr.contains("schema.#Base")
            && stderr.contains("doesn't")
            && stderr.contains("support tasks"),
        "error message should explain Base schema doesn't support tasks, got: {stderr}",
    );
}
