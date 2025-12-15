#![allow(missing_docs)]

use cuengine::evaluate_cue_package_typed;
use cuenv_core::manifest::{Base, Cuenv};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

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
    fs::create_dir_all(&schema_dst).unwrap();
    for entry in fs::read_dir(&schema_src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("cue") {
            continue;
        }
        let file_name = path.file_name().unwrap();
        fs::copy(&path, schema_dst.join(file_name)).unwrap();
    }
}

#[test]
fn project_name_is_required_by_schema() {
    let tmp = TempDir::new().unwrap();
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

    let res = evaluate_cue_package_typed::<Cuenv>(root, "cuenv");
    assert!(res.is_err(), "schema should reject missing `name`");
}

#[test]
fn project_name_cannot_be_empty() {
    let tmp = TempDir::new().unwrap();
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

    let res = evaluate_cue_package_typed::<Cuenv>(root, "cuenv");
    assert!(res.is_err(), "schema should reject empty `name`");
}

#[test]
fn base_can_be_composed_standalone() {
    let tmp = TempDir::new().unwrap();
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
    let tmp = TempDir::new().unwrap();
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
    let output = Command::new("cargo")
        .args(["run", "--bin", "cuenv", "--"])
        .args(["task", "--path", root.to_str().unwrap()])
        .output()
        .expect("Failed to run cuenv");

    let stderr = String::from_utf8_lossy(&output.stderr);
    
    assert!(!output.status.success(), "should fail with Base schema");
    assert!(
        stderr.contains("No project found in current directory"),
        "error message should mention no project found, got: {}",
        stderr
    );
}
